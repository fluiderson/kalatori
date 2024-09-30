//! Database definitions from tables to their keys & values.
//!
//! Ideally, this module should contain only primitive types & structs with them to be always
//! stable since primitive types don't change their representation.

pub use v1::{
    AssetId, BlockHash, ChainHash, ChainProperties, DaemonInfo, KeysTable, NonZeroU64, OrderId,
    OrderInfo, OrdersPerChainTable, OrdersTable, PaymentStatus, Public, RootKey, RootTable,
    RootValue, TableRead, TableTrait, TableTypes, TableWrite, Timestamp, Version,
};

mod v1 {
    use crate::{
        chain_wip::definitions::{BlockHash as ChainBlockHash, H256, H64},
        error::{DbError, TimestampError},
    };
    use ahash::{HashMap, HashSet};
    use arrayvec::{ArrayString, ArrayVec};
    use codec::{Decode, Encode, Error as CodecError, Input};
    use redb::{
        AccessGuard, Key, ReadOnlyTable, ReadTransaction, ReadableTable, Table, TableDefinition,
        TableError, TypeName, Value, WriteTransaction,
    };
    use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize};
    use std::{
        borrow::Borrow,
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

    macro_rules! table_types {
        ($table:ident<$key:ident, $value:ident>) => {
            impl TableTypes for $table {
                type Key = $key;
                type Value = $value;
            }
        };

        ($table:ident<$key:ident<'_>, $value:ident>) => {
            impl TableTypes for $table {
                type Key = $key<'static>;
                type Value = $value;
            }
        };

        ($table:ident<$key:ident, $value:ident<'_>>) => {
            impl TableTypes for $table {
                type Key = $key;
                type Value = $value<'static>;
            }
        };
    }

    pub trait TableTrait: TableTypes {
        const NAME: &'static str;
        const DEFINITION: TableDefinition<'static, Self::Key, Self::Value> =
            TableDefinition::new(Self::NAME);

        #[expect(clippy::type_complexity)]
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

    macro_rules! table {
        ($table:ident<$key:ident, $value:ident> = $name:literal) => {
            pub struct $table;

            table_types!($table<$key, $value>);

            impl TableTrait for $table {
                const NAME: &'static str = $name;
            }
        };

        ($table:ident<$key:ident<'_>, $value:ident> = $name:literal) => {
            pub struct $table;

            table_types!($table<$key<'_>, $value>);

            impl TableTrait for $table {
                const NAME: &'static str = $name;
            }
        };

        ($table:ident<$key:ident, $value:ident<'_>> = $name:literal) => {
            pub struct $table;

            table_types!($table<$key, $value<'_>>);

            impl TableTrait for $table {
                const NAME: &'static str = $name;
            }
        };
    }

    pub trait TableRead<K: Key + 'static, V: Value + 'static>: ReadableTable<K, V> {
        fn get_slot<'a>(
            &self,
            key: impl Borrow<K::SelfType<'a>>,
        ) -> Result<Option<AccessGuard<'_, V>>, DbError> {
            self.get(key).map_err(DbError::Get)
        }
    }

    impl<K: Key + 'static, V: Value + 'static, T: ReadableTable<K, V>> TableRead<K, V> for T {}

    pub trait TableWrite<'t, 'tx: 't, K: Key + 'static, V: Value + 'static> {
        fn table(&mut self) -> &mut Table<'tx, K, V>;

        fn insert_slot<'a>(
            &'t mut self,
            key: impl Borrow<K::SelfType<'a>>,
            value: impl Borrow<V::SelfType<'a>>,
        ) -> Result<Option<AccessGuard<'t, V>>, DbError> {
            self.table().insert(key, value).map_err(DbError::Insert)
        }
    }

    impl<'t, 'tx: 't, K: Key + 'static, V: Value + 'static> TableWrite<'t, 'tx, K, V>
        for Table<'tx, K, V>
    {
        fn table(&mut self) -> &mut Table<'tx, K, V> {
            self
        }
    }

    // Unused but should stay as a possible mechanism for chain table trees.
    #[expect(unused)]
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
            name.try_push_str(&H64::from_be_bytes(chain.0).to_hex())
                .unwrap();

            tx.open_table(TableDefinition::new(&name))
                .map_err(DbError::OpenTable)
        }

        /// An utility function for the database cleanup.
        ///
        /// Returns [`true`] if provided `maybe_hash` has an invalid format, or is unknown for the
        /// daemon (i.e. `chain_hashes` don't have `maybe_hash`).
        fn try_open<'a>(
            maybe_hash: &str,
            chain_hashes: &HashSet<ChainHash>,
            tx: &'a WriteTransaction,
            tables: &mut HashMap<ChainHash, Table<'a, Self::Key, Self::Value>>,
        ) -> Result<bool, DbError> {
            let Ok(hash) = H64::from_hex(maybe_hash) else {
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

    // Unused but should stay as a possible mechanism for chain table trees.
    #[expect(unused)]
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

    #[expect(edition_2024_expr_fragment_specifier)]
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

        ($name:ident<'_>(&[u8])) => {
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

    #[expect(edition_2024_expr_fragment_specifier)]
    macro_rules! key_slot {
        ($name:ident([u8; $length:expr])) => {
            slot!($name([u8; $length]));

            impl Key for $name {
                fn compare(former: &[u8], latter: &[u8]) -> Ordering {
                    former.cmp(latter)
                }
            }
        };

        ($name:ident<'_>(&[u8])) => {
            slot!($name<'_>(&[u8]));

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

    table!(RootTable<RootKey, RootValue<'_>> = "root");
    table!(KeysTable<Public, NonZeroU64> = "keys");
    table!(OrdersPerChainTable<ChainHash, NonZeroU64> = "orders_per_chain");
    table!(OrdersTable<OrderId<'_>, OrderInfo> = "orders");

    // Slots

    #[derive(Debug, Decode, Encode)]
    pub struct OrderInfo {
        pub withdrawal_status: WithdrawalStatus,
        pub payment_status: PaymentStatus,
        pub amount: Amount,
        pub message: Vec<u8>,
        pub currency: CurrencyInfo,
        pub callback: Vec<u8>,
        pub transactions: Vec<TransactionInfo>,
        pub payment_account: Account,
        pub death: Timestamp,
    }

    scale_slot!(OrderInfo);

    #[derive(Debug, Decode, Encode)]
    pub enum WithdrawalStatus {
        Waiting,
        Failed,
        Completed,
        None,
    }

    #[derive(Debug, Decode, Encode, PartialEq, Eq)]
    pub enum PaymentStatus {
        Pending,
        Paid,
        TimedOut,
    }

    #[derive(Debug, Decode, Encode)]
    pub struct CurrencyInfo {
        pub kind: TokenKind,
        pub asset_id: Option<AssetId>,
    }

    #[derive(Debug, Decode, Encode)]
    pub enum TokenKind {
        Asset,
        Native,
    }

    #[derive(Debug, Decode, Encode)]
    pub struct TransactionInfo {
        finalized_tx: Option<FinalizedTx>,
        transaction_bytes: String,
        sender: Account,
        recipient: Account,
        amount: Amount,
        status: TxStatus,
    }

    #[derive(Debug, Decode, Encode)]
    struct FinalizedTx {
        block_number: BlockNumber,
        position_in_block: ExtrinsicIndex,
        timestamp: Timestamp,
    }

    #[derive(Debug, Decode, Encode)]
    enum TxStatus {
        Pending,
        Finalized,
        Failed,
    }

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

    #[derive(Debug, Encode, Decode)]
    pub struct ExtrinsicIndex(pub u32);
    #[derive(Debug, Encode, Decode, PartialEq, Eq, Hash, Clone, Copy)]
    pub struct Public(pub [u8; 32]);
    #[derive(Debug, Deserialize, Clone, Copy, Decode, Encode)]
    pub struct BlockNumber(u32);
    #[derive(Debug, Deserialize, Clone, Copy, Decode, Encode)]
    pub struct AssetId(pub u32);
    #[derive(Debug, Clone, Copy)]
    pub struct OrderId<'a>(&'a [u8]);
    #[derive(Debug, Encode, Decode)]
    pub struct Account([u8; 32]);
    #[derive(Debug, PartialEq)]
    pub struct Version(pub u64);
    #[derive(Debug)]
    pub struct RootValue<'a>(pub &'a [u8]);
    #[derive(Debug, Encode, Decode, PartialEq, Clone, Copy)]
    pub struct BlockHash(pub [u8; 32]);
    #[derive(Debug, Encode, Decode, PartialEq, Eq, Hash, Clone, Copy)]
    pub struct ChainHash(pub [u8; 8]);
    #[derive(Debug, Encode, Decode, PartialEq, Eq, Clone, Copy)]
    pub struct Amount(pub u128);

    key_slot!(Public([u8; 32]));
    key_slot!(BlockNumber(u32));
    key_slot!(OrderId<'_>(&[u8]));
    key_slot!(Account([u8; 32]));
    key_slot!(ChainHash([u8; 8]));
    slot!(Version(u64));
    slot!(RootValue<'_>(&[u8]));
    slot!(BlockHash([u8; 32]));
    slot!(AssetId(u32));

    impl<'a> From<&'a str> for OrderId<'a> {
        fn from(value: &'a str) -> Self {
            Self(value.as_bytes())
        }
    }

    impl<'a> From<OrderId<'a>> for &'a str {
        fn from(value: OrderId<'a>) -> Self {
            str::from_utf8(value.0).unwrap()
        }
    }

    impl From<ChainBlockHash> for BlockHash {
        fn from(hash: ChainBlockHash) -> Self {
            Self(hash.0.to_be_bytes())
        }
    }

    impl From<H64> for ChainHash {
        fn from(hash: H64) -> Self {
            Self(hash.to_be_bytes())
        }
    }

    impl From<ChainHash> for H64 {
        fn from(value: ChainHash) -> Self {
            H64::from_be_bytes(value.0)
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

    #[derive(Encode, Debug, Clone, Copy, Serialize)]
    pub struct Timestamp(#[codec(compact)] u64);

    impl Decode for Timestamp {
        fn decode<I: Input>(input: &mut I) -> Result<Self, CodecError> {
            let millis = u64::decode(input)?;

            Self::from_millis(millis).map_err(|e| {
                const ERROR: &str = concat!("failed to decode `", stringify!(Timestamp), "`");

                tracing::debug!("{ERROR}: {e}");

                CodecError::from(ERROR)
            })
        }
    }

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
                #[expect(clippy::cast_possible_truncation)]
                Ok(Self(millis as u64))
            }
        }

        /// Saturating. Should be used to create a timestamp for the display purpose only.
        #[expect(unused)]
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
                .field("hash", &H64::from_be_bytes(self.hash.0))
                .finish()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::Timestamp;
        use std::time::SystemTime;
        use time::{format_description::well_known::Rfc3339, OffsetDateTime};

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
