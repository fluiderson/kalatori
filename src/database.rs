use crate::{
    rpc::{ChainProperties, EndpointProperties},
    Account, Asset, Balance, BlockNumber, Decimals, Nonce, RuntimeConfig, Version, ASSET,
    DATABASE_VERSION, SEED,
};
use anyhow::{Context, Result};
use redb::{
    backends::InMemoryBackend, AccessGuard, Database, ReadOnlyTable, ReadableTable, RedbKey,
    RedbValue, Table, TableDefinition, TableHandle, TypeName,
};
use std::{borrow::Borrow, fmt::Write, sync::Arc};
use subxt::{
    ext::{
        codec::{Compact, Decode, Encode},
        sp_core::{
            crypto::Ss58Codec,
            sr25519::{Pair, Public},
            DeriveJunction, Pair as _,
        },
    },
    tx::PairSigner,
    utils::KeyedVec,
    Config,
};
use tokio::{
    sync::{RwLock, RwLockReadGuard},
    task,
};

pub const MODULE: &str = module_path!();

// Tables

const ROOT: TableDefinition<'_, &str, Vec<u8>> = TableDefinition::new("root");
const INVOICES: TableDefinition<'_, &str, Invoice> = TableDefinition::new("invoices");
const LIVE_ACCOUNTS: TableDefinition<'_, &AccountK, &str> = TableDefinition::new("live_accounts");
const DEAD_ACCOUNTS: TableDefinition<'_, &AccountK, &Derivation> =
    TableDefinition::new("dead_accounts");
const TRANSACTIONS: TableDefinition<'_, &AccountK, TransferTransaction> =
    TableDefinition::new("transactions");
const PENDING_ACTIONS: TableDefinition<'_, BlockNumber, Actions> =
    TableDefinition::new("pending_actions");
const HIT_LIST: TableDefinition<'_, BlockNumber, &AccountK> = TableDefinition::new("hit_list");

// Keys

// The database version must be stored in a separate slot to be used by the not implemented yet
// database migration logic.
const DB_VERSION_KEY: &str = "db_version";
const DAEMON_INFO: &str = "daemon_info";
const LAST_BLOCK: &str = "last_block";

// Slots

type Derivation = [u8; 32];
type AccountK = [u8; 32];
type Actions = KeyedVec<&'static AccountK, Vec<Transfer>>;
type Transfer = (&'static AccountK, Balance);
type TransferTransaction = (Nonce, Transfer);

#[derive(Debug, Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
pub struct Invoice {
    pub derivation: Derivation,
    pub status: InvoiceStatus,
}

impl Invoice {
    pub fn signer(&self, pair: &Pair) -> Result<PairSigner<RuntimeConfig, Pair>> {
        // let invoice_pair = pair
        //     .derive(
        //         [self.recipient.clone().into(), self.order]
        //             .map(DeriveJunction::Hard)
        //             .into_iter(),
        //         None,
        //     )
        //     .context("failed to derive an invoice key pair")?
        //     .0;

        // Ok(PairSigner::new(invoice_pair))

        todo!()
    }
}

#[derive(Debug, Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
pub enum InvoiceStatus {
    Unpaid(Balance),
    Paid,
}

impl RedbValue for Invoice {
    type SelfType<'a> = Self;

    type AsBytes<'a> = Vec<u8>;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(mut encoded_invoice: &[u8]) -> Self::SelfType<'_>
    where
        Self: 'a,
    {
        Self::decode(&mut encoded_invoice).unwrap()
    }

    fn as_bytes<'a, 'b: 'a>(invoice: &'a Self::SelfType<'_>) -> Self::AsBytes<'a> {
        invoice.encode()
    }

    fn type_name() -> TypeName {
        TypeName::new(stringify!(Invoice))
    }
}

#[derive(Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
struct DaemonInfo {
    rpc: String,
    key: Public,
    asset: Option<Asset>,
}

pub struct State {
    database: Database,
    chain: Arc<RwLock<ChainProperties>>,
    rpc: String,
    properties: StateConfig,
}

impl State {
    pub fn initialise(
        db_path_option: Option<String>,
        EndpointProperties { url, chain }: EndpointProperties,
        config: StateConfig,
        purge_block: BlockNumber,
    ) -> Result<(Arc<Self>, Option<BlockNumber>)> {
        let public = config.pair.public();
        let public_formatted = public.to_ss58check_with_version(
            task::block_in_place(|| chain.blocking_read()).address_format,
        );
        let given_rpc = url.get();

        let mut database = if let Some(path) = db_path_option {
            log::info!("Creating/Opening the database at \"{path}\".");

            redb::Database::create(path)
        } else {
            log::warn!(
                "The in-memory backend for the database is selected. All saved data will be deleted after the daemon shutdown!"
            );

            redb::Database::builder().create_with_backend(InMemoryBackend::new())
        }.context("failed to create/open the database")?;

        let tx = begin_write_tx(&database)?;
        let mut root = tx.root()?;

        let last_block = match (
            root.get_slot(DB_VERSION_KEY)?,
            root.get_slot(DAEMON_INFO)?,
            root.get_slot(LAST_BLOCK)?,
        ) {
            (None, None, None) => {
                root.insert_db_version()?;
                root.insert_daemon_info(given_rpc.clone(), public, config.asset)?;

                None
            }
            (Some(encoded_db_version), Some(daemon_info), last_block_option) => {
                let Compact::<Version>(db_version) =
                    decode_slot(encoded_db_version, DB_VERSION_KEY)?;

                if db_version != DATABASE_VERSION {
                    anyhow::bail!(
                        "database contains an unsupported database version (\"{db_version}\"), expected \"{DATABASE_VERSION}\""
                    );
                }

                let DaemonInfo { rpc: db_rpc, key, asset: db_asset } = decode_slot(daemon_info, DAEMON_INFO)?;

                if public != key {
                    anyhow::bail!(
                        "public key from `{SEED}` doesn't equal the one from the database (\"{public_formatted}\")"
                    );
                }

                match (config.asset, db_asset) {
                    (None, Some(db_id)) =>
                        anyhow::bail!(
                            "database was created for the asset {db_id} but the native token was given since `{ASSET}` isn't set"
                        ),
                    (Some(given_id), None) =>
                        anyhow::bail!(
                            "database was created for the native token but the asset {given_id} was given in `{ASSET}`"
                        ),
                    (Some(given_id), Some(db_id)) if given_id != db_id =>
                        anyhow::bail!(
                            "database was created for the asset {db_id} but the asset {given_id} was given in `{ASSET}`"
                        ),
                    _ => {}
                }

                if given_rpc != db_rpc {
                    log::warn!("The saved RPC endpoint ({db_rpc:?}) differs from the given one ({given_rpc:?}) and will be overwritten with it.");

                    root.insert_daemon_info(given_rpc.clone(), public, config.asset)?;
                }

                if let Some(encoded_last_block) = last_block_option {
                    Some(decode_slot::<Compact<BlockNumber>>(encoded_last_block, LAST_BLOCK)?.0)
                } else {
                    None
                }
            }
            _ => anyhow::bail!(
                "database was found but it doesn't contain `{DB_VERSION_KEY:?}` and/or `{DAEMON_INFO:?}`, maybe it was created by another program"
            ),
        };

        tx.pending_actions()?.purge(purge_block)?;

        let mut hit_list = tx.hit_list()?;
        let mut live_accounts = tx.live_accounts()?;
        let mut dead_accounts = tx.dead_accounts()?;
        let mut invoices = tx.invoices()?;

        for account_result in hit_list.purge(purge_block)? {
            let guard = account_result?;
            let account = guard.value();

            let invoice_key = live_accounts.remove(account)?;
            let invoice = invoices.remove(invoice_key.value())?.value();

            dead_accounts.insert(account, &invoice.derivation)?;
        }

        drop((root, hit_list, live_accounts, dead_accounts, invoices));

        tx.commit()?;

        let compacted = database
            .compact()
            .context("failed to compact the database")?;

        if compacted {
            log::debug!("The database was successfully compacted.")
        } else {
            log::debug!("The database doesn't need the compaction.")
        }

        log::info!("Public key from the given seed: \"{public_formatted}\".");
        log::info!("Decimals: {}.", config.decimals);
        log::info!(
            "Expected fee maximum: {}.",
            format_balance_with_decimals(config.fee, config.decimals)
        );

        if let Some(id) = config.asset {
            log::info!("The asset {id} is selected as the asset type.");
        } else {
            log::info!("The native token is selected as the asset type.");
        }

        Ok((
            Arc::new(Self {
                database,
                chain,
                rpc: given_rpc,
                properties: config,
            }),
            last_block,
        ))
    }

    pub fn rpc(&self) -> &str {
        &self.rpc
    }

    pub fn destination(&self) -> &Option<Account> {
        // &self.destination

        todo!()
    }

    pub fn write(&self) -> Result<WriteTransaction<'_>> {
        begin_write_tx(&self.database)
    }

    pub fn read(&self) -> Result<ReadTransaction<'_>> {
        self.database
            .begin_read()
            .map(ReadTransaction)
            .context("failed to begin a read transaction for the database")
    }

    pub async fn properties(&self) -> RwLockReadGuard<'_, ChainProperties> {
        self.chain.read().await
    }

    pub fn pair(&self) -> &Pair {
        &self.properties.pair
    }
}

pub struct StateConfig {
    pub pair: Pair,
    pub recipient: Account,
    pub asset: Option<Asset>,
    pub fee: Balance,
    pub decimals: Decimals,
}

fn begin_write_tx(database: &Database) -> Result<WriteTransaction<'_>> {
    database
        .begin_write()
        .map(WriteTransaction)
        .context("failed to begin a write transaction for the database")
}

pub struct ReadTransaction<'db>(redb::ReadTransaction<'db>);

impl ReadTransaction<'_> {
    pub fn invoices(&self) -> Result<ReadInvoices<'_>> {
        // self.0
        //     .open_table(INVOICES)
        //     .map(ReadInvoices)
        //     .with_context(|| format!("failed to open the `{}` table", INVOICES.name()))

        todo!()
    }
}

pub struct ReadInvoices<'tx>(ReadOnlyTable<'tx, &'static [u8; 32], Invoice>);

impl ReadInvoices<'_> {
    pub fn get(&self, account: &Account) -> Result<Option<AccessGuard<'_, Invoice>>> {
        self.0
            .get(AsRef::<[u8; 32]>::as_ref(account))
            .context("failed to get an invoice from the database")
    }
}

pub struct WriteTransaction<'db>(redb::WriteTransaction<'db>);

impl<'db> WriteTransaction<'db> {
    pub fn root(&self) -> Result<Root<'db, '_>> {
        self.open_table(ROOT).map(Root)
    }

    pub fn hit_list(&self) -> Result<HitList<'db, '_>> {
        self.open_table(HIT_LIST).map(HitList)
    }

    pub fn pending_actions(&self) -> Result<PendingActions<'db, '_>> {
        self.open_table(PENDING_ACTIONS).map(PendingActions)
    }

    pub fn live_accounts(&self) -> Result<LiveAccounts<'db, '_>> {
        self.open_table(LIVE_ACCOUNTS).map(LiveAccounts)
    }

    pub fn dead_accounts(&self) -> Result<DeadAccounts<'db, '_>> {
        self.open_table(DEAD_ACCOUNTS).map(DeadAccounts)
    }

    pub fn invoices(&self) -> Result<WriteInvoices<'db, '_>> {
        self.open_table(INVOICES).map(WriteInvoices)
    }

    pub fn commit(self) -> Result<()> {
        self.0
            .commit()
            .context("failed to commit a write transaction in the database")
    }

    fn open_table<K: RedbKey, V: RedbValue>(
        &self,
        table: TableDefinition<'_, K, V>,
    ) -> Result<Table<'db, '_, K, V>> {
        self.0
            .open_table(table)
            .with_context(|| format!("failed to open the {:?} table", table.name()))
    }
}

pub struct Root<'db, 'tx>(Table<'db, 'tx, &'static str, Vec<u8>>);

impl Root<'_, '_> {
    pub fn save_last_block(&mut self, number: BlockNumber) -> Result<()> {
        self.insert_slot(LAST_BLOCK, Compact(number))
    }

    fn get_slot(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.0
            .get(key)
            .map(|slot_option| slot_option.map(|slot| slot.value().to_vec()))
            .with_context(|| format!("failed to get the {key:?} slot"))
    }

    fn insert_db_version(&mut self) -> Result<()> {
        self.insert_slot(DB_VERSION_KEY, Compact(DATABASE_VERSION))
    }

    fn insert_daemon_info(&mut self, rpc: String, key: Public, asset: Option<Asset>) -> Result<()> {
        self.insert_slot(DAEMON_INFO, DaemonInfo { rpc, key, asset })
    }

    fn insert_slot(&mut self, key: &str, value: impl Encode) -> Result<()> {
        self.0
            .insert(key, value.encode())
            .map(|_| ())
            .with_context(|| format!("failed to insert the {key:?} slot"))
    }
}

fn decode_slot<T: Decode>(slot: Vec<u8>, key: &str) -> Result<T> {
    T::decode(&mut slot.as_ref()).with_context(|| format!("failed to decode the {key:?} slot"))
}

pub struct HitList<'db, 'tx>(Table<'db, 'tx, BlockNumber, &'static AccountK>);

impl<'db, 'tx> HitList<'db, 'tx> {
    fn purge(
        &mut self,
        purge_block: BlockNumber,
    ) -> Result<impl Iterator<Item = Result<AccessGuard<'_, &'static AccountK>>>> {
        self.0
            .drain(..=purge_block)
            .context("failed to purge outdated live accounts")
            .map(|drain| {
                drain.map(|item| {
                    item.with_context(|| {
                        format!(
                            "failed to get an account from the {:?} table",
                            HIT_LIST.name()
                        )
                    })
                    .map(|(_, account)| account)
                })
            })
    }
}

pub struct PendingActions<'db, 'tx>(Table<'db, 'tx, BlockNumber, Actions>);

impl PendingActions<'_, '_> {
    fn purge(&mut self, purge_block: BlockNumber) -> Result<()> {
        self.0
            .drain(..=purge_block)
            .map(|_| ())
            .context("failed to purge outdated pending actions")
    }
}

pub struct DeadAccounts<'db, 'tx>(Table<'db, 'tx, &'static AccountK, &'static Derivation>);

impl DeadAccounts<'_, '_> {
    pub fn insert(&mut self, account: &AccountK, derivation: &Derivation) -> Result<()> {
        self.0
            .insert(account, derivation)
            .with_context(|| {
                format!(
                    "failed to insert an account into the {:?} table",
                    DEAD_ACCOUNTS.name()
                )
            })
            .map(|_| ())
    }
}

pub struct LiveAccounts<'db, 'tx>(Table<'db, 'tx, &'static AccountK, &'static str>);

impl LiveAccounts<'_, '_> {
    pub fn remove(&mut self, account: &AccountK) -> Result<AccessGuard<'_, &'static str>> {
        remove_slot(&mut self.0, account, "an account", LIVE_ACCOUNTS)
    }
}

pub struct WriteInvoices<'db, 'tx>(Table<'db, 'tx, &'static str, Invoice>);

impl WriteInvoices<'_, '_> {
    pub fn remove(&mut self, key: &str) -> Result<AccessGuard<'_, Invoice>> {
        remove_slot(&mut self.0, key, "an invoice", INVOICES)
    }
}

fn remove_slot<'a, 'b, K: RedbKey, V: RedbValue>(
    table: &'a mut Table<'_, '_, K, V>,
    key: impl Borrow<K::SelfType<'b>>,
    value: &str,
    table_definition: TableDefinition<'_, K, V>,
) -> Result<AccessGuard<'a, V>> {
    table
        .remove(key)
        .with_context(|| {
            format!(
                "failed to remove {value} from the {:?} table",
                table_definition.name()
            )
        })
        .and_then(|option_guard| {
            option_guard.with_context(|| {
                format!(
                    "failed to get {value} from the {:?} table",
                    table_definition.name()
                )
            })
        })
}

fn format_balance_with_decimals(balance: Balance, decimals: Decimals) -> String {
    todo!()
}
