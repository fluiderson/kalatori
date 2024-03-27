use crate::{rpc::ConnectedChain, AssetId, Balance, BlockNumber, Nonce, Timestamp};
use anyhow::{Context, Result};
use redb::{Database, ReadableTable, Table, TableDefinition, TableHandle, TypeName, Value};
use std::{collections::HashMap, sync::Arc};
use subxt::ext::{
    codec::{Compact, Decode, Encode},
    sp_core::sr25519::{Pair, Public},
};

pub const MODULE: &str = module_path!();

// Tables

const ROOT: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("root");
const KEYS: TableDefinition<'_, PublicSlot, U256Slot> = TableDefinition::new("keys");
const CHAINS: TableDefinition<'_, ChainHash, BlockNumber> = TableDefinition::new("chains");
const INVOICES: TableDefinition<'_, InvoiceKey, Invoice> = TableDefinition::new("invoices");
const HIT_LIST: TableDefinition<'_, Timestamp, (ChainHash, AssetId, Account)> =
    TableDefinition::new("hit_list");

const ACCOUNTS: &str = "accounts";

type ACCOUNTS_KEY = (Option<AssetId>, Account);
type ACCOUNTS_VALUE = InvoiceKey;

const TRANSACTIONS: &str = "transactions";

type TRANSACTIONS_KEY = BlockNumber;
type TRANSACTIONS_VALUE = (Account, Nonce, Transfer);

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
    asset: Option<Compact<AssetId>>,
    transactions: Vec<TransferTx>,
}

#[derive(Encode, Decode, Debug)]
#[codec(crate = subxt::ext::codec)]
struct TransferTx {
    encoded: Vec<u8>,
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

pub struct State {
    db: Database,
    // properties: Arc<RwLock<ChainProperties>>,
    // pair: Pair,
    // rpc: String,
    // destination: Option<Account>,
}

impl State {
    pub fn initialise(
        path_option: Option<String>,
        current_pair: (Pair, Public),
        old_pairs: HashMap<String, (Pair, Public)>,
        connected_chains: HashMap<String, ConnectedChain>,
    ) -> Result<Arc<Self>> {
        // let mut database = Database::create(path_option.unwrap()).unwrap();

        // let tx = database
        //     .begin_write()
        //     .context("failed to begin a write transaction")?;
        // let mut table = tx
        //     .open_table(ROOT)
        //     .with_context(|| format!("failed to open the `{}` table", ROOT.name()))?;
        // drop(
        //     tx.open_table(INVOICES)
        //         .with_context(|| format!("failed to open the `{}` table", INVOICES.name()))?,
        // );

        //         let last_block = match (
        //             get_slot(&table, DB_VERSION_KEY)?,
        //             get_slot(&table, DAEMON_INFO)?,
        //             get_slot(&table, LAST_BLOCK)?,
        //         ) {
        //             (None, None, None) => {
        //                 table
        //                     .insert(
        //                         DB_VERSION_KEY,
        //                         Compact(DATABASE_VERSION).encode(),
        //                     )
        //                     .context("failed to insert the database version")?;
        //                 insert_daemon_info(&mut table, given_rpc.clone(), public)?;

        //                 None
        //             }
        //             (Some(encoded_db_version), Some(daemon_info), last_block_option) => {
        //                 let Compact::<Version>(db_version) =
        //                     decode_slot(&encoded_db_version, DB_VERSION_KEY)?;
        //                 let DaemonInfo { rpc: db_rpc, key } = decode_slot(&daemon_info, DAEMON_INFO)?;

        //                 if db_version != DATABASE_VERSION {
        //                     anyhow::bail!(
        //                         "database contains an unsupported database version (\"{db_version}\"), expected \"{DATABASE_VERSION}\""
        //                     );
        //                 }

        //                 if public != key {
        //                     anyhow::bail!(
        //                         "public key from `{SEED}` doesn't equal the one from the database (\"{public_formatted}\")"
        //                     );
        //                 }

        //                 if given_rpc != db_rpc {
        //                     if override_rpc {
        //                         log::warn!(
        //                             "The saved RPC endpoint ({db_rpc:?}) differs from the given one ({given_rpc:?}) and will be overwritten by it because `{OVERRIDE_RPC}` is set."
        //                         );

        //                         insert_daemon_info(&mut table, given_rpc.clone(), public)?;
        //                     } else {
        //                         anyhow::bail!(
        //                             "database contains a different RPC endpoint address ({db_rpc:?}), expected {given_rpc:?}"
        //                         );
        //                     }
        //                 } else if override_rpc {
        //                     log::warn!(
        //                         "`{OVERRIDE_RPC}` is set but the saved RPC endpoint ({db_rpc:?}) equals to the given one."
        //                     );
        //                 }

        //                 if let Some(encoded_last_block) = last_block_option {
        //                     Some(decode_slot::<Compact<BlockNumber>>(&encoded_last_block, LAST_BLOCK)?.0)
        //                 } else {
        //                     None
        //                 }
        //             }
        //             _ => anyhow::bail!(
        //                 "database was found but it doesn't contain `{DB_VERSION_KEY:?}` and/or `{DAEMON_INFO:?}`, maybe it was created by another program"
        //             ),
        //         };

        //         drop(table);

        //         tx.commit().context("failed to commit a transaction")?;

        //         let compacted = database
        //             .compact()
        //             .context("failed to compact the database")?;

        //         if compacted {
        //             log::debug!("The database was successfully compacted.");
        //         } else {
        //             log::debug!("The database doesn't need the compaction.");
        //         }

        //         log::info!("Public key from the given seed: \"{public_formatted}\".");

        // Ok((
        //     Arc::new(Self {
        //         db: database,
        //         properties: chain,
        //         pair,
        //         rpc: given_rpc,
        //         destination,
        //     }),
        //     last_block,
        // ))

        todo!()
    }

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
