use crate::{BlockNumber, Timestamp};
use redb::TableDefinition;
use subxt::ext::codec::{Compact, Decode, Encode};

// Tables

pub const ROOT: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("root");
pub const KEYS: TableDefinition<'_, PublicSlot, U256Slot> = TableDefinition::new("keys");
pub const CHAINS: TableDefinition<'_, ChainHash, BlockNumber> = TableDefinition::new("chains");
// const INVOICES: TableDefinition<'_, InvoiceKey, Invoice> = TableDefinition::new("invoices");

// const ACCOUNTS: &str = "accounts";

// type ACCOUNTS_KEY = Account;
// type ACCOUNTS_VALUE = (InvoiceKey, Nonce);

// const TRANSACTIONS: &str = "transactions";

// type TRANSACTIONS_KEY = BlockNumber;
// type TRANSACTIONS_VALUE = (Account, Transfer);

// const HIT_LIST: &str = "hit_list";

// type HIT_LIST_KEY = BlockNumber;
// type HIT_LIST_VALUE = (Option<AssetId>, Account);

// `ROOT` keys

// The database version must be stored in a separate slot.
pub const DB_VERSION_KEY: &str = "db_version";
pub const DAEMON_INFO: &str = "daemon_info";

// Slots

pub type PublicSlot = [u8; 32];
pub type InvoiceKey = &'static [u8];
pub type U256Slot = [u64; 4];
pub type BlockHash = [u8; 32];
pub type ChainHash = [u8; 32];
pub type BalanceSlot = u128;
pub type Derivation = [u8; 32];
pub type Account = [u8; 32];

#[derive(Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
pub struct DaemonInfo {
    pub chains: Vec<(String, ChainProperties)>,
    pub public: PublicSlot,
    pub old_publics_death_timestamps: Vec<(PublicSlot, Compact<Timestamp>)>,
}

#[derive(Encode, Decode, Clone, PartialEq, Debug)]
#[codec(crate = subxt::ext::codec)]
pub struct ChainProperties {
    pub genesis: BlockHash,
    pub hash: ChainHash,
}

// #[derive(Encode, Decode)]
// #[codec(crate = subxt::ext::codec)]
// struct Transfer(Option<Compact<AssetId>>, #[codec(compact)] BalanceSlot);

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
