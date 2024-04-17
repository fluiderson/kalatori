use crate::{
    rpc::{ConnectedChain, Currency},
    server::{CurrencyProperties, ServerInfo},
    AccountId, AssetId, Balance, BlockNumber, Config, Nonce, Timestamp, Version,
};
use anyhow::{Context, Result};
use redb::{
    backends::{FileBackend, InMemoryBackend},
    Database, ReadableTable, Table, TableDefinition, TableHandle, TypeName, Value,
};
use serde::Deserialize;
use std::{collections::HashMap, fs::File, io::ErrorKind, sync::Arc};
use subxt::ext::{
    codec::{Compact, Decode, Encode},
    sp_core::{
        crypto::Ss58Codec,
        sr25519::{Pair, Public},
    },
};
use tokio::sync::RwLock;

pub const MODULE: &str = module_path!();

// Tables

const ROOT: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("root");
const KEYS: TableDefinition<'_, PublicSlot, U256Slot> = TableDefinition::new("keys");
const CHAINS: TableDefinition<'_, ChainHash, BlockNumber> = TableDefinition::new("chains");
const INVOICES: TableDefinition<'_, InvoiceKey, Invoice> = TableDefinition::new("invoices");

const ACCOUNTS: &str = "accounts";

type ACCOUNTS_KEY = (Option<AssetId>, Account);
type ACCOUNTS_VALUE = InvoiceKey;

const TRANSACTIONS: &str = "transactions";

type TRANSACTIONS_KEY = BlockNumber;
type TRANSACTIONS_VALUE = (Account, Nonce, Transfer);

const HIT_LIST: &str = "hit_list";

type HIT_LIST_KEY = BlockNumber;
type HIT_LIST_VALUE = (Option<AssetId>, Account);

// `ROOT` keys

// The database version must be stored in a separate slot to be used by the not implemented yet
// database migration logic.
const DB_VERSION_KEY: &str = "db_version";
const DAEMON_INFO: &str = "daemon_info";

// Slots

type InvoiceKey = &'static [u8];
type U256Slot = [u64; 4];
type BlockHash = [u8; 32];
type ChainHash = [u8; 32];
type PublicSlot = [u8; 32];
type BalanceSlot = u128;
type Derivation = [u8; 32];
type Account = [u8; 32];

#[derive(Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
enum ChainKind {
    Id(Vec<Compact<AssetId>>),
    MultiLocation(Vec<Compact<AssetId>>),
}

#[derive(Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
struct DaemonInfo {
    chains: Vec<(String, ChainProperties)>,
    current_key: PublicSlot,
    old_keys_death_timestamps: Vec<(PublicSlot, Timestamp)>,
}

#[derive(Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
struct ChainProperties {
    genesis: BlockHash,
    hash: ChainHash,
    kind: ChainKind,
}

#[derive(Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
struct Transfer(Option<Compact<AssetId>>, #[codec(compact)] BalanceSlot);

#[derive(Encode, Decode, Debug)]
#[codec(crate = subxt::ext::codec)]
struct Invoice {
    derivation: (PublicSlot, Derivation),
    paid: bool,
    #[codec(compact)]
    timestamp: Timestamp,
    #[codec(compact)]
    price: BalanceSlot,
    callback: String,
    message: String,
    transactions: TransferTxs,
}

#[derive(Encode, Decode, Debug)]
#[codec(crate = subxt::ext::codec)]
enum TransferTxs {
    Asset {
        #[codec(compact)]
        id: AssetId,
        // transactions: TransferTxsAsset,
    },
    Native {
        recipient: Account,
        encoded: Vec<u8>,
        exact_amount: Option<Compact<BalanceSlot>>,
    },
}

// #[derive(Encode, Decode, Debug)]
// #[codec(crate = subxt::ext::codec)]
// struct TransferTxsAsset<T> {
//     recipient: Account,
//     encoded: Vec<u8>,
//     #[codec(compact)]
//     amount: BalanceSlot,
// }

#[derive(Encode, Decode, Debug)]
#[codec(crate = subxt::ext::codec)]
struct TransferTx {
    recipient: Account,
    exact_amount: Option<Compact<BalanceSlot>>,
}

impl Value for Invoice {
    type SelfType<'a> = Self;

    type AsBytes<'a> = Vec<u8>;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(mut data: &[u8]) -> Self::SelfType<'_>
    where
        Self: 'a,
    {
        Self::decode(&mut data).unwrap()
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'a>) -> Self::AsBytes<'_> {
        value.encode()
    }

    fn type_name() -> TypeName {
        TypeName::new(stringify!(Invoice))
    }
}

pub struct ConfigWoChains {
    pub recipient: String,
    pub debug: bool,
    pub remark: String,
    pub depth: Option<BlockNumber>,
    pub account_lifetime: BlockNumber,
    pub rpc: String,
}

pub struct State {
    pub currencies: HashMap<String, CurrencyProperties>,
    pub recipient: AccountId,
    pub pair: Pair,
    pub depth: Option<Timestamp>,
    pub account_lifetime: Timestamp,
    pub debug: bool,
    pub remark: String,
    pub invoices: RwLock<HashMap<String, Invoicee>>,
    pub rpc: String,
}

#[derive(Deserialize, Debug)]
pub struct Invoicee {
    pub callback: String,
    pub amount: Balance,
    pub paid: bool,
    pub paym_acc: AccountId,
}

impl State {
    pub fn initialise(
        path_option: Option<String>,
        currencies: HashMap<String, CurrencyProperties>,
        current_pair: Pair,
        old_pairs: HashMap<String, Pair>,
        ConfigWoChains {
            recipient,
            debug,
            remark,
            depth,
            account_lifetime,
            rpc,
        }: ConfigWoChains,
    ) -> Result<Arc<Self>> {
        let builder = Database::builder();
        let is_new;

        let database = if let Some(path) = path_option {
            tracing::info!("Creating/Opening the database at {path:?}.");

            match File::create_new(&path) {
                Ok(file) => {
                    is_new = true;

                    FileBackend::new(file).and_then(|backend| builder.create_with_backend(backend))
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    is_new = false;

                    builder.create(path)
                }
                Err(error) => Err(error.into())
            }
        } else {
            tracing::warn!(
                "The in-memory backend for the database is selected. All saved data will be deleted after the shutdown!"
            );

            is_new = true;

            builder.create_with_backend(InMemoryBackend::new())
        }.context("failed to create/open the database")?;

        //

        Ok(Arc::new(Self {
            currencies,
            recipient: AccountId::from_string(&recipient)
                .context("failed to convert \"recipient\" from the config to an account address")?,
            pair: current_pair,
            depth,
            account_lifetime,
            debug,
            remark,

            invoices: RwLock::new(HashMap::new()),
            rpc,
        }))
    }

    pub fn server_info(&self) -> ServerInfo {
        ServerInfo {
            version: env!("CARGO_PKG_VERSION"),
            instance_id: String::new(),
            debug: self.debug,
            kalatori_remark: self.remark.clone(),
        }
    }
/*
    pub fn currency(&self, currency_name: &str) -> Option<Currency> {
        if let Some(currency) = self.currencies.get(currency_name) {
            Some(Currency {
                chain: currency.chain_name.clone(),
                asset: currency.asset_id,
            })
        } else { None }
    }
*/
    //     pub fn rpc(&self) -> &str {
    //         &self.rpc
    //     }

    //     pub fn destination(&self) -> &Option<Account> {
    //         &self.destination
    //     }

    //     pub fn write(&self) -> Result<WriteTransaction<'_>> {
    //         self.db
    //             .begin_write()
    //             .map(WriteTransaction)
    //             .context("failed to begin a write transaction for the database")
    //     }

    //     pub fn read(&self) -> Result<ReadTransaction<'_>> {
    //         self.db
    //             .begin_read()
    //             .map(ReadTransaction)
    //             .context("failed to begin a read transaction for the database")
    //     }

    //     pub async fn properties(&self) -> RwLockReadGuard<'_, ChainProperties> {
    //         self.properties.read().await
    //     }

    //     pub fn pair(&self) -> &Pair {
    //         &self.pair
    //     }
}

// pub struct ReadTransaction<'db>(redb::ReadTransaction<'db>);

// impl ReadTransaction<'_> {
//     pub fn invoices(&self) -> Result<ReadInvoices<'_>> {
//         self.0
//             .open_table(INVOICES)
//             .map(ReadInvoices)
//             .with_context(|| format!("failed to open the `{}` table", INVOICES.name()))
//     }
// }

// pub struct ReadInvoices<'tx>(ReadOnlyTable<'tx, &'static [u8; 32], Invoice>);

// impl ReadInvoices<'_> {
//     pub fn get(&self, account: &Account) -> Result<Option<AccessGuard<'_, Invoice>>> {
//         self.0
//             .get(AsRef::<[u8; 32]>::as_ref(account))
//             .context("failed to get an invoice from the database")
//     }

//     pub fn try_iter(
//         &self,
//     ) -> Result<impl Iterator<Item = Result<(AccessGuard<'_, &[u8; 32]>, AccessGuard<'_, Invoice>)>>>
//     {
//         self.0
//             .iter()
//             .context("failed to get the invoices iterator")
//             .map(|iter| iter.map(|item| item.context("failed to get an invoice from the iterator")))
//     }
// }

// pub struct WriteTransaction<'db>(redb::WriteTransaction<'db>);

// impl<'db> WriteTransaction<'db> {
//     pub fn root(&self) -> Result<Root<'db, '_>> {
//         self.0
//             .open_table(ROOT)
//             .map(Root)
//             .with_context(|| format!("failed to open the `{}` table", ROOT.name()))
//     }

//     pub fn invoices(&self) -> Result<WriteInvoices<'db, '_>> {
//         self.0
//             .open_table(INVOICES)
//             .map(WriteInvoices)
//             .with_context(|| format!("failed to open the `{}` table", INVOICES.name()))
//     }

//     pub fn commit(self) -> Result<()> {
//         self.0
//             .commit()
//             .context("failed to commit a write transaction in the database")
//     }
// }

// pub struct WriteInvoices<'db, 'tx>(Table<'db, 'tx, &'static [u8; 32], Invoice>);

// impl WriteInvoices<'_, '_> {
//     pub fn save(
//         &mut self,
//         account: &Account,
//         invoice: &Invoice,
//     ) -> Result<Option<AccessGuard<'_, Invoice>>> {
//         self.0
//             .insert(AsRef::<[u8; 32]>::as_ref(account), invoice)
//             .context("failed to save an invoice in the database")
//     }
// }

// pub struct Root<'db, 'tx>(Table<'db, 'tx, &'static str, Vec<u8>>);

// impl Root<'_, '_> {
//     pub fn save_last_block(&mut self, number: BlockNumber) -> Result<()> {
//         self.0
//             .insert(LAST_BLOCK, Compact(number).encode())
//             .context("context")?;

//         Ok(())
//     }
// }

// fn get_slot(table: &Table<'_, &str, Vec<u8>>, key: &str) -> Result<Option<Vec<u8>>> {
//     table
//         .get(key)
//         .map(|slot_option| slot_option.map(|slot| slot.value().clone()))
//         .with_context(|| format!("failed to get the {key:?} slot"))
// }

// fn decode_slot<T: Decode>(mut slot: &[u8], key: &str) -> Result<T> {
//     T::decode(&mut slot).with_context(|| format!("failed to decode the {key:?} slot"))
// }

// fn insert_daemon_info(
//     table: &mut Table<'_, '_, &str, Vec<u8>>,
//     rpc: String,
//     key: Public,
// ) -> Result<()> {
//     table
//         .insert(DAEMON_INFO, DaemonInfo { rpc, key }.encode())
//         .map(|_| ())
//         .context("failed to insert the daemon info")
// }
