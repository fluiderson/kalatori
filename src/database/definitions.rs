pub use v0::OrdersTable;
pub use v1::{
    Asset, BlockHash, BlockNumber, ChainHash, ChainProperties, ChainTableTrait, DaemonInfo,
    KeysTable, Public, RootKey, RootTable, RootValue, TableRead, TableTrait, TableTypes,
    TableWrite, Timestamp, Version,
};

mod v0 {
    use super::{TableTrait, TableTypes};
    use crate::definitions::api_v2::OrderInfo;
    use codec::{Decode, Encode};
    use redb::{TypeName, Value};

    crate::table!(OrdersTable<&'static str, OrderInfo> = "orders");
    crate::scale_slot!(OrderInfo);
}

mod v1 {
    use crate::{
        chain::definitions::{BlockHash as ChainBlockHash, H256},
        error::{DbError, TimestampError},
    };
    use ahash::{HashMap, HashSet};
    use arrayvec::{ArrayString, ArrayVec};
    use codec::{Decode, Encode};
    use redb::{
        AccessGuard, Key, ReadOnlyTable, ReadTransaction, ReadableTable, Table, TableDefinition,
        TableError, TypeName, Value, WriteTransaction,
    };
    use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize};
    use std::{
        cmp::Ordering,
        fmt::{Debug, Display, Formatter, Result as FmtResult},
        num::NonZeroU64 as StdNonZeroU64,
        str,
        time::SystemTime,
    };
    use substrate_crypto_light::sr25519::Public as CryptoPublic;
    use time::{format_description::well_known::Rfc3339, Duration, OffsetDateTime};

    // Traits & macros

    pub trait TableTypes {
        type Key: Key + 'static;
        type Value: Value + 'static;
    }

    #[macro_export]
    macro_rules! table_types {
        ($table:ident<$key:ty, $value:ty>) => {
            impl TableTypes for $table {
                type Key = $key;
                type Value = $value;
            }
        };
    }

    pub trait TableTrait: TableTypes {
        const NAME: &'static str;
        const DEFINITION: TableDefinition<'static, Self::Key, Self::Value> =
            TableDefinition::new(Self::NAME);

        #[allow(clippy::type_complexity)]
        fn open_ro(
            tx: &ReadTransaction,
        ) -> Result<Option<ReadOnlyTable<Self::Key, Self::Value>>, DbError> {
            tx.open_table(Self::DEFINITION)
                .map(Some)
                .or_else(|error| {
                    if matches!(error, TableError::TableDoesNotExist(_)) {
                        Ok(None)
                    } else {
                        Err(error)
                    }
                })
                .map_err(DbError::OpenTable)
        }

        fn open(tx: &WriteTransaction) -> Result<Table<'_, Self::Key, Self::Value>, DbError> {
            tx.open_table(Self::DEFINITION).map_err(DbError::OpenTable)
        }
    }

    #[macro_export]
    macro_rules! table {
        ($table:ident<$key:ty, $value:ty> = $name:literal) => {
            pub struct $table;

            $crate::table_types!($table<$key, $value>);

            impl TableTrait for $table {
                const NAME: &'static str = $name;
            }
        };
    }

    pub trait TableRead {
        type Table: TableTrait;

        fn get_slot<'a>(
            table: &'a impl ReadableTable<
                <Self::Table as TableTypes>::Key,
                <Self::Table as TableTypes>::Value,
            >,
            key: &<<Self::Table as TableTypes>::Key as Value>::SelfType<'_>,
        ) -> Result<Option<AccessGuard<'a, <Self::Table as TableTypes>::Value>>, DbError> {
            table.get(key).map_err(DbError::Get)
        }
    }

    pub trait TableWrite: TableRead {
        fn insert_slot(
            table: &mut Table<
                '_,
                <Self::Table as TableTypes>::Key,
                <Self::Table as TableTypes>::Value,
            >,
            key: &<<Self::Table as TableTypes>::Key as Value>::SelfType<'_>,
            value: &<<Self::Table as TableTypes>::Value as Value>::SelfType<'_>,
        ) -> Result<(), DbError> {
            table
                .insert(key, value)
                .map(|_| ())
                .map_err(DbError::Insert)
        }
    }

    // TODO: `<const N: usize>` is redundant.
    // https://github.com/rust-lang/rust/issues/76560
    pub trait ChainTableTrait<const N: usize>: TableTypes {
        const PREFIX: &'static str;

        fn open(
            tx: &WriteTransaction,
            chain: ChainHash,
        ) -> Result<Table<'_, Self::Key, Self::Value>, DbError> {
            let mut name = ArrayString::<N>::new();

            name.try_push_str(Self::PREFIX).unwrap();
            name.try_push_str(&H256::from_be_bytes(chain.0).to_hex())
                .unwrap();

            tx.open_table(TableDefinition::new(&name))
                .map_err(DbError::OpenTable)
        }

        fn try_open<'a>(
            maybe_hash: &str,
            chain_hashes: &HashSet<ChainHash>,
            tx: &'a WriteTransaction,
            tables: &mut HashMap<ChainHash, Table<'a, Self::Key, Self::Value>>,
        ) -> Result<bool, DbError> {
            let Ok(hash) = H256::from_hex(maybe_hash) else {
                return Ok(true);
            };

            let chain = hash.into();

            if !chain_hashes.contains(&chain) {
                return Ok(true);
            }

            tables.insert(chain, Self::open(tx, chain)?);

            Ok(false)
        }
    }

    macro_rules! chain_table {
        ($table:ident<$key:ty, $value:ty> = $prefix:literal) => {
            pub struct $table;

            table_types!($table<$key, $value>);

            impl ChainTableTrait<{ $prefix.len() + H256::HEX_LENGTH }> for $table {
                const PREFIX: &'static str = $prefix;
            }
        };
    }

    #[macro_export]
    macro_rules! scale_slot {
        ($name:ident) => {
            impl Value for $name {
                type SelfType<'a> = Self;
                type AsBytes<'a> = Vec<u8>;

                fn fixed_width() -> Option<usize> {
                    None
                }

                fn from_bytes<'a>(mut data: &[u8]) -> Self
                where
                    Self: 'a,
                {
                    Self::decode(&mut data).unwrap()
                }

                fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'_>) -> Self::AsBytes<'a> {
                    Self::encode(value)
                }

                fn type_name() -> TypeName {
                    TypeName::new(stringify!($name))
                }
            }
        };
    }

    macro_rules! slot {
        ($name:ident([u8; $length:expr])) => {
            impl Value for $name {
                type SelfType<'a> = Self;
                type AsBytes<'a> = &'a [u8; $length];

                fn fixed_width() -> Option<usize> {
                    Some($length)
                }

                fn from_bytes<'a>(data: &[u8]) -> Self
                where
                    Self: 'a,
                {
                    Self(*<&[u8; $length]>::from_bytes(data))
                }

                fn as_bytes<'a, 'b: 'a>(value: &Self) -> Self::AsBytes<'_> {
                    &value.0
                }

                fn type_name() -> TypeName {
                    TypeName::new(stringify!($name))
                }
            }
        };

        ($name:ident<'a>(&'a [u8])) => {
            impl Value for $name<'_> {
                type SelfType<'a> = $name<'a> where Self: 'a;
                type AsBytes<'a> = &'a [u8] where Self: 'a;

                fn fixed_width() -> Option<usize> {
                    None
                }

                fn from_bytes<'a>(data: &'a [u8]) -> Self::SelfType<'_>
                where
                    Self: 'a,
                {
                    $name(data)
                }

                fn as_bytes<'a, 'b: 'a>(value: &Self::SelfType<'b>) -> &'a [u8]
                where
                    Self: 'b,
                {
                    value.0
                }

                fn type_name() -> TypeName {
                    TypeName::new(stringify!($name))
                }
            }
        };

        ($name:ident($inner_type:ty)) => {
            impl Value for $name {
                type SelfType<'a> = Self;
                type AsBytes<'a> = <$inner_type as Value>::AsBytes<'a>;

                fn fixed_width() -> Option<usize> {
                    <$inner_type>::fixed_width()
                }

                fn from_bytes<'a>(data: &[u8]) -> Self
                where
                    Self: 'a,
                {
                    Self(<$inner_type>::from_bytes(data))
                }

                fn as_bytes<'a, 'b: 'a>(value: &Self) -> Self::AsBytes<'_> {
                    <$inner_type>::as_bytes(&value.0)
                }

                fn type_name() -> TypeName {
                    TypeName::new(stringify!($name))
                }
            }
        };
    }

    macro_rules! key_slot {
        ($name:ident([u8; $length:expr])) => {
            slot!($name([u8; $length]));

            impl Key for $name {
                fn compare(former: &[u8], latter: &[u8]) -> Ordering {
                    former.cmp(latter)
                }
            }
        };

        ($name:ident<'a>(&'a [u8])) => {
            slot!($name<'a>(&'a [u8]));

            impl Key for $name<'_> {
                fn compare(former: &[u8], latter: &[u8]) -> Ordering {
                    former.cmp(latter)
                }
            }
        };

        ($name:ident($inner_type:ty)) => {
            slot!($name($inner_type));

            impl Key for $name {
                fn compare(former: &[u8], latter: &[u8]) -> Ordering {
                    Self::from_bytes(former).0.cmp(&Self::from_bytes(latter).0)
                }
            }
        };
    }

    // Tables

    table!(RootTable<RootKey, RootValue<'static>> = "root");
    table!(KeysTable<Public, NonZeroU64> = "keys");
    // table!(InvoicesTable<InvoiceKey<'static>, Invoice> = "invoices");
    // pub const INVOICES_NAME: &str = "invoices";
    // pub const INVOICES: TableDefinition<'_, InvoiceKey, Invoice> =
    //     TableDefinition::new(INVOICES_NAME);

    // chain_table!(AccountsTable<Account, InvoiceKey<'static>> = "accounts");
    // chain_table!(HitListTable<BlockNumber, InvoiceKey<'static>> = "hit_list");

    // // const TRANSACTIONS: &str = "transactions";

    // // type TRANSACTIONS_KEY = BlockNumber;
    // // type TRANSACTIONS_VALUE = (Account, Transfer);

    // // const HIT_LIST: &str = "hit_list";

    // // type HIT_LIST_KEY = BlockNumber;
    // // type HIT_LIST_VALUE = (Option<AssetId>, Account);

    // Slots

    #[derive(Debug)]
    pub struct NonZeroU64(pub StdNonZeroU64);

    impl Value for NonZeroU64 {
        type SelfType<'a> = Self;

        type AsBytes<'a> = <u64 as Value>::AsBytes<'a>;

        fn fixed_width() -> Option<usize> {
            u64::fixed_width()
        }

        fn from_bytes<'a>(data: &[u8]) -> Self
        where
            Self: 'a,
        {
            Self(StdNonZeroU64::new(u64::from_bytes(data)).unwrap())
        }

        fn as_bytes<'a, 'b: 'a>(value: &Self) -> Self::AsBytes<'_> {
            u64::as_bytes(&value.0.get())
        }

        fn type_name() -> TypeName {
            TypeName::new(stringify!(NonZeroU64))
        }
    }
    #[derive(Debug)]
    pub enum RootKey {
        // The database version must be stored in a separate slot.
        DbVersion,
        DaemonInfo,
    }

    impl RootKey {
        const DB_VERSION: &'static [u8] = b"db_version";
        const DAEMON_INFO: &'static [u8] = b"daemon_info";
    }

    impl Key for RootKey {
        fn compare(former: &[u8], latter: &[u8]) -> Ordering {
            former.cmp(latter)
        }
    }

    impl Value for RootKey {
        type SelfType<'a> = Self;

        type AsBytes<'a> = &'static [u8];

        fn fixed_width() -> Option<usize> {
            None
        }

        fn from_bytes<'a>(data: &[u8]) -> Self
        where
            Self: 'a,
        {
            match data {
                Self::DAEMON_INFO => Self::DaemonInfo,
                Self::DB_VERSION => Self::DbVersion,
                _ => unreachable!(),
            }
        }

        fn as_bytes<'a, 'b: 'a>(value: &Self) -> Self::AsBytes<'a>
        where
            Self: 'a,
            Self: 'b,
        {
            match value {
                RootKey::DbVersion => Self::DB_VERSION,
                RootKey::DaemonInfo => Self::DAEMON_INFO,
            }
        }

        fn type_name() -> TypeName {
            TypeName::new(stringify!(RootKey))
        }
    }

    #[derive(Debug, Encode, Decode, PartialEq, Eq, Hash, Clone, Copy)]
    pub struct Public(pub [u8; 32]);
    #[derive(Debug, Deserialize, Clone, Copy)]
    pub struct BlockNumber(u32);
    #[derive(Debug, Deserialize, Clone, Copy)]
    pub struct Asset(pub u32);
    #[derive(Debug)]
    pub struct InvoiceKey<'a>(&'a [u8]);
    #[derive(Debug)]
    pub struct Account([u8; 32]);
    #[derive(Debug, PartialEq)]
    pub struct Version(pub u64);
    #[derive(Debug)]
    pub struct RootValue<'a>(pub &'a [u8]);
    #[derive(Debug, Encode, Decode, PartialEq, Clone, Copy)]
    pub struct BlockHash(pub [u8; 32]);
    #[derive(Debug, Encode, Decode, PartialEq, Eq, Hash, Clone, Copy)]
    pub struct ChainHash(pub [u8; 32]);

    key_slot!(Public([u8; 32]));
    key_slot!(BlockNumber(u32));
    key_slot!(InvoiceKey<'a>(&'a [u8]));
    key_slot!(Account([u8; 32]));
    slot!(Version(u64));
    slot!(RootValue<'a>(&'a [u8]));
    slot!(BlockHash([u8; 32]));
    slot!(ChainHash([u8; 32]));
    slot!(Asset(u32));

    // pub type BalanceSlot = u128;
    // pub type Derivation = [u8; 32];

    impl From<ChainBlockHash> for BlockHash {
        fn from(hash: ChainBlockHash) -> Self {
            Self(hash.0.to_be_bytes())
        }
    }

    impl From<H256> for ChainHash {
        fn from(hash: H256) -> Self {
            Self(hash.to_be_bytes())
        }
    }

    impl From<CryptoPublic> for Public {
        fn from(public: CryptoPublic) -> Self {
            Self(public.0)
        }
    }

    macro_rules! into_h256 {
        ($from:ty) => {
            impl From<$from> for H256 {
                fn from(value: $from) -> Self {
                    H256::from_be_bytes(value.0)
                }
            }
        };
    }

    into_h256!(Public);
    into_h256!(BlockHash);
    into_h256!(ChainHash);

    #[derive(Encode, Decode)]
    pub struct DaemonInfo {
        pub chains: Vec<(String, ChainProperties)>,
        pub public: Public,
        pub old_publics_death_timestamps: Vec<(Public, Timestamp)>,
        pub instance: Vec<u8>,
    }

    impl<'de> Deserialize<'de> for Timestamp {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            Self::from_millis(Deserialize::deserialize(deserializer)?).map_err(DeError::custom)
        }
    }

    #[derive(Encode, Decode, Debug, Clone, Copy, Serialize)]
    pub struct Timestamp(#[codec(compact)] u64);

    impl Timestamp {
        pub const MAX: u64 = 253_402_300_799_999;
        pub const MAX_STRING: &'static str = "9999-12-31T23:59:59.999999999Z";

        pub fn as_millis(self) -> u64 {
            self.0
        }

        pub fn from_millis(millis: u64) -> Result<Self, TimestampError> {
            if millis > Self::MAX {
                Err(TimestampError::Overflow)
            } else {
                Ok(Self(millis))
            }
        }

        pub fn now() -> Result<Self, TimestampError> {
            let millis = get_system_millis()?;

            if millis > Self::MAX.into() {
                Err(TimestampError::Overflow)
            } else {
                // Shouldn't truncate as `mills <= Self::MAX`.
                #[allow(clippy::cast_possible_truncation)]
                Ok(Self(millis as u64))
            }
        }

        /// Saturating. Should be used to create a timestamp for the display purpose only.
        pub fn from_duration_in_millis(millis: u64) -> Self {
            let Ok(system_ms) = get_system_millis().unwrap_or_default().try_into() else {
                return Self(Self::MAX);
            };

            let unchecked = millis.saturating_add(system_ms);

            Self(if unchecked > Self::MAX {
                Self::MAX
            } else {
                unchecked
            })
        }
    }

    fn get_system_millis() -> Result<u128, TimestampError> {
        Ok(SystemTime::UNIX_EPOCH.elapsed()?.as_millis())
    }

    impl Display for Timestamp {
        fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
            let mut string = ArrayVec::<_, { Self::MAX_STRING.len() }>::new();

            OffsetDateTime::UNIX_EPOCH
                .saturating_add(Duration::milliseconds(self.0.try_into().unwrap()))
                .format_into(&mut string, &Rfc3339)
                .unwrap();

            f.write_str(str::from_utf8(&string).unwrap())
        }
    }

    #[derive(Encode, Decode)]
    pub struct ChainProperties {
        pub genesis: BlockHash,
        pub hash: ChainHash,
    }

    impl Debug for ChainProperties {
        fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
            f.debug_struct(stringify!(ChainProperties))
                .field("genesis", &H256::from_be_bytes(self.genesis.0))
                .field("hash", &H256::from_be_bytes(self.hash.0))
                .finish()
        }
    }

    // // #[derive(Encode, Decode)]
    // // #[codec(crate = subxt::ext::codec)]
    // // struct Transfer(Option<Compact<AssetId>>, #[codec(compact)] BalanceSlot);

    #[derive(Encode, Decode, Debug)]
    pub struct Invoice {
        pub paid: bool,
        pub dasd: [u8; 32],
    }

    scale_slot!(Invoice);

    // impl Value for Invoice {
    //     type SelfType<'a> = Self;

    //     type AsBytes<'a> = Vec<u8>;

    //     fn fixed_width() -> Option<usize> {
    //         None
    //     }

    //     fn from_bytes<'a>(mut data: &[u8]) -> Self::SelfType<'_>
    //     where
    //         Self: 'a,
    //     {
    //         Self::decode(&mut data).unwrap()
    //     }

    //     fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'a>) -> Self::AsBytes<'_> {
    //         value.encode()
    //     }

    //     fn type_name() -> TypeName {
    //         TypeName::new(stringify!(Invoice))
    //     }
    // }

    // #[derive(Encode, Decode, Debug)]
    // #[codec(crate = subxt::ext::codec)]
    // struct Invoice {
    //     derivation: (PublicSlot, Derivation),
    //     paid: bool,
    //     #[codec(compact)]
    //     timestamp: Timestamp,
    //     #[codec(compact)]
    //     price: BalanceSlot,
    //     callback: String,
    //     message: String,
    //     transactions: TransferTxs,
    // }

    // #[derive(Encode, Decode, Debug)]
    // #[codec(crate = subxt::ext::codec)]
    // enum TransferTxs {
    //     Asset {
    //         #[codec(compact)]
    //         id: AssetId,
    //         // transactions: TransferTxsAsset,
    //     },
    //     Native {
    //         recipient: Account,
    //         encoded: Vec<u8>,
    //         exact_amount: Option<Compact<BalanceSlot>>,
    //     },
    // }

    // #[derive(Encode, Decode, Debug)]
    // #[codec(crate = subxt::ext::codec)]
    // struct TransferTxsAsset<T> {
    //     recipient: Account,
    //     encoded: Vec<u8>,
    //     #[codec(compact)]
    //     amount: BalanceSlot,
    // }

    // #[derive(Encode, Decode, Debug)]
    // #[codec(crate = subxt::ext::codec)]
    // struct TransferTx {
    //     recipient: Account,
    //     exact_amount: Option<Compact<BalanceSlot>>,
    // }

    // pub struct ConfigWoChains {
    //     pub recipient: String,
    //     pub debug: Option<bool>,
    //     pub remark: Option<String>,
    //     pub depth: Option<BlockNumber>,
    //     pub account_lifetime: BlockNumber,
    //     pub rpc: String,
    // }

    // #[derive(Deserialize, Debug)]
    // pub struct Invoicee {
    //     pub callback: String,
    //     pub amount: Balance,
    //     pub paid: bool,
    //     pub paym_acc: AccountId,
    // }

    #[cfg(test)]
    mod tests {
        use std::time::SystemTime;

        use time::{format_description::well_known::Rfc3339, OffsetDateTime};

        use super::Timestamp;

        #[test]
        fn timestamp_max() {
            let expected_max: SystemTime = OffsetDateTime::parse(Timestamp::MAX_STRING, &Rfc3339)
                .unwrap()
                .into();
            let expected_max_in_ms: u64 = expected_max
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                .try_into()
                .unwrap();

            assert_eq!(Timestamp::MAX, expected_max_in_ms);
        }
    }
}
