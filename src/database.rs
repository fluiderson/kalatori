use crate::{
    rpc::{ConnectedChain, Currency},
    AccountId, AssetId, Balance, BlockNumber, Config, Nonce, Timestamp, Version, DB_VERSION,
    OLD_SEED, SEED,
};
use anyhow::{Context, Result};
use hex_simd::AsciiCase;
use redb::{
    backends::{FileBackend, InMemoryBackend},
    AccessGuard, Database, ReadOnlyTable, ReadableTable, Table, TableDefinition, TableError,
    TableHandle, TypeName, Value,
};
use serde::Deserialize;
use std::{
    array,
    collections::{hash_map::Entry, HashMap, HashSet},
    fs::File,
    io::ErrorKind,
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime},
};
use subxt::ext::{
    codec::{Compact, Decode, Encode, EncodeLike},
    sp_core::{
        crypto::Ss58Codec,
        sr25519::{Pair, Public},
        Pair as _, U256,
    },
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::sync::RwLock;

pub const MODULE: &str = module_path!();

type CheckedChains = HashMap<Rc<String>, (Option<ConnectedChain>, ChainHash)>;

// Tables

const ROOT: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("root");
const KEYS: TableDefinition<'_, PublicSlot, U256Slot> = TableDefinition::new("keys");
const CHAINS: TableDefinition<'_, ChainHash, BlockNumber> = TableDefinition::new("chains");
const INVOICES: TableDefinition<'_, InvoiceKey, Invoice> = TableDefinition::new("invoices");

const ACCOUNTS: &str = "accounts";

type ACCOUNTS_KEY = Account;
type ACCOUNTS_VALUE = (InvoiceKey, Nonce);

const TRANSACTIONS: &str = "transactions";

type TRANSACTIONS_KEY = BlockNumber;
type TRANSACTIONS_VALUE = (Account, Transfer);

const HIT_LIST: &str = "hit_list";

type HIT_LIST_KEY = BlockNumber;
type HIT_LIST_VALUE = (Option<AssetId>, Account);

// `ROOT` keys

// The database version must be stored in a separate slot.
const DB_VERSION_KEY: &str = "db_version";
const DAEMON_INFO: &str = "daemon_info";

// Slots

pub type PublicSlot = [u8; 32];

type InvoiceKey = &'static [u8];
type U256Slot = [u64; 4];
type BlockHash = [u8; 32];
type ChainHash = [u8; 32];
type BalanceSlot = u128;
type Derivation = [u8; 32];
type Account = [u8; 32];

#[derive(Encode, Decode)]
#[codec(crate = subxt::ext::codec)]
struct DaemonInfo {
    chains: Vec<(String, ChainProperties)>,
    public: PublicSlot,
    old_publics_death_timestamps: Vec<(PublicSlot, Compact<Timestamp>)>,
}

#[derive(Encode)]
#[codec(crate = subxt::ext::codec)]
struct DaemonInfoRef {
    chains: Vec<(Rc<String>, ChainProperties)>,
    public: PublicSlot,
    old_publics_death_timestamps: Vec<(PublicSlot, Compact<Timestamp>)>,
}

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
#[codec(crate = subxt::ext::codec)]
struct ChainProperties {
    genesis: BlockHash,
    hash: ChainHash,
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
    pub debug: Option<bool>,
    pub remark: Option<String>,
    pub depth: Option<BlockNumber>,
    pub account_lifetime: BlockNumber,
    pub rpc: String,
}

pub struct State {
    // currencies: HashMap<String, Currency>,
    recipient: AccountId,
    pair: Pair,
    // old_pairs: HashMap<PublicSlot, Pair>,
    // depth: Option<Timestamp>,
    account_lifetime: Timestamp,
    debug: Option<bool>,
    remark: Option<String>,
    // invoices: RwLock<HashMap<String, Invoicee>>,
}

#[derive(Deserialize, Debug)]
pub struct Invoicee {
    pub callback: String,
    pub amount: Balance,
    pub paid: bool,
    pub paym_acc: AccountId,
}

impl State {
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        path_option: Option<String>,
        debug: Option<bool>,
        recipient: &str,
        remark: Option<String>,
        pair: Pair,
        old_pairs: HashMap<PublicSlot, (Pair, String)>,
        account_lifetime: Timestamp,
        chains: HashMap<Arc<String>, ConnectedChain>,
    ) -> Result<(Arc<Self>, CheckedChains)> {
        let builder = Database::builder();

        let (mut database, is_new) = if let Some(path) = path_option {
            tracing::info!("Creating/Opening the database at {path:?}.");

            match File::create_new(&path) {
                Ok(file) => FileBackend::new(file)
                    .and_then(|backend| builder.create_with_backend(backend))
                    .map(|db| (db, true)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => builder
                    .create(path)
                    .map(|db| (db, false)),
                Err(error) => Err(error.into())
            }
        } else {
            tracing::warn!(
                "The in-memory backend for the database is selected. All saved data will be deleted after the shutdown!"
            );

            builder.create_with_backend(InMemoryBackend::new()).map(|db| (db, true))
        }.context("failed to create/open the database")?;

        let tx = database
            .begin_write()
            .context("failed to begin a read transaction")?;
        let mut root = tx.open_table(ROOT).with_context(|| {
            format!(
                "failed to open the {:?} table in a write transaction",
                ROOT.name()
            )
        })?;
        let public = pair.public().0;
        let mut checked_chains = HashMap::with_capacity(chains.len());

        let ro_root_option = database
            .begin_read()
            .context("failed to begin a read transaction")?
            .open_table(ROOT)
            .map(Some)
            .or_else(|error| {
                if matches!(error, TableError::TableDoesNotExist(_)) {
                    Ok(None)
                } else {
                    Err(error)
                }
            })
            .with_context(|| {
                format!(
                    "failed to open the {:?} table in a read transaction",
                    ROOT.name()
                )
            })?;
        let root_slots = match &ro_root_option {
            Some(ro_root) => Some((
                get_slot(ro_root, DB_VERSION_KEY)?,
                get_slot(ro_root, DAEMON_INFO)?,
            )),
            None => None,
        };

        let daemon_info;

        if let Some((Some(encoded_db_version), Some(encoded_daemon_info))) = root_slots {
            daemon_info = process_existing_db(
                &encoded_db_version,
                &encoded_daemon_info,
                public,
                old_pairs,
                chains,
                &mut checked_chains,
            )?;
        } else {
            daemon_info = process_empty_db(is_new, chains, &mut checked_chains, &mut root, public)?;
        }

        root.insert(DAEMON_INFO, daemon_info.as_slice())
            .with_context(|| format!("failed to save {DAEMON_INFO:?}"))?;

        drop(root);

        tx.commit()
            .context("failed to commit a write transaction to the database")?;

        if database
            .compact()
            .context("failed to compact the database")?
        {
            tracing::debug!("The database has been compacted.");
        } else {
            tracing::debug!("The database doesn't need to be compacted.");
        }

        Ok((
            Arc::new(Self {
                recipient: AccountId::from_string(recipient).context(
                    "failed to convert `recipient` from the config to an account address",
                )?,
                pair,
                account_lifetime,
                debug,
                remark,
            }),
            checked_chains,
        ))
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

fn process_existing_db(
    encoded_db_version: &AccessGuard<'_, &'static [u8]>,
    encoded_daemon_info: &AccessGuard<'_, &'static [u8]>,
    public: PublicSlot,
    old_pairs: HashMap<PublicSlot, (Pair, String)>,
    mut chains: HashMap<Arc<String>, ConnectedChain>,
    checked_chains: &mut CheckedChains,
) -> Result<Vec<u8>> {
    let db_version: Version = decode_slot(encoded_db_version, DB_VERSION_KEY)?;

    if db_version != DB_VERSION {
        anyhow::bail!(
            "database contains an invalid database version ({db_version}), expected {DB_VERSION}"
        );
    }

    let DaemonInfo {
        chains: db_chains,
        public: mut db_public,
        old_publics_death_timestamps,
    } = decode_slot(encoded_daemon_info, DAEMON_INFO)?;

    let (checked_old_pairs, new_old_publics_death_timestamps) = process_keys(
        public,
        &mut db_public,
        old_pairs,
        old_publics_death_timestamps,
    )?;

    let capacity = db_chains.len().saturating_add(chains.len());
    let mut new_db_chains = Vec::with_capacity(capacity);
    let mut chain_hashes = HashSet::with_capacity(capacity);

    for (name, properties) in db_chains {
        process_db_chain(
            &mut chains,
            name,
            properties,
            &mut new_db_chains,
            &mut chain_hashes,
            checked_chains,
        )?;
    }

    for (name, chain) in chains {
        process_chain(
            name,
            chain,
            &mut chain_hashes,
            checked_chains,
            &mut new_db_chains,
        );
    }

    Ok(DaemonInfoRef {
        chains: new_db_chains,
        public,
        old_publics_death_timestamps: new_old_publics_death_timestamps,
    }
    .encode())
}

fn process_empty_db(
    is_new: bool,
    chains: HashMap<Arc<String>, ConnectedChain>,
    checked_chains: &mut CheckedChains,
    root: &mut Table<'_, &str, &[u8]>,
    public: PublicSlot,
) -> Result<Vec<u8>> {
    if !is_new {
        anyhow::bail!(
            "existing database doesn't contain {DB_VERSION_KEY:?} and/or {DAEMON_INFO:?}, maybe it was created by another program"
        );
    }

    let capacity = chains.len();
    let mut new_db_chains = Vec::with_capacity(capacity);
    let mut chain_hashes = HashSet::with_capacity(capacity);

    for (name, chain) in chains {
        process_chain(
            name,
            chain,
            &mut chain_hashes,
            checked_chains,
            &mut new_db_chains,
        );
    }

    root.insert(DB_VERSION_KEY, DB_VERSION.encode().as_slice())
        .with_context(|| format!("failed to save {DB_VERSION_KEY:?} in the database"))?;

    Ok(DaemonInfoRef {
        chains: new_db_chains,
        public,
        old_publics_death_timestamps: vec![],
    }
    .encode())
}

fn get_slot<'a>(
    table: &'a ReadOnlyTable<&str, &[u8]>,
    key: &str,
) -> Result<Option<AccessGuard<'a, &'static [u8]>>> {
    table
        .get(key)
        .with_context(|| format!("failed to get the {key:?} slot"))
}

fn decode_slot<T: Decode>(slot: &AccessGuard<'_, &[u8]>, key: &str) -> Result<T> {
    T::decode(&mut slot.value()).with_context(|| format!("failed to decode the {key:?} slot"))
}

fn encode_to_hex(data: impl AsRef<[u8]>) -> String {
    const PREFIX: &str = "0x";

    let data = data.as_ref();
    let mut string = String::with_capacity(PREFIX.len() + data.len());

    string.push_str(PREFIX);

    hex_simd::encode_append(data, &mut string, AsciiCase::Lower);

    string
}

fn format_timestamp(timestamp: Timestamp) -> String {
    const MAX: &str = "9999-12-31T23:59:59.999999999Z";

    OffsetDateTime::UNIX_EPOCH
        .saturating_add(
            time::Duration::try_from(Duration::from_micros(timestamp))
                .unwrap_or(time::Duration::MAX),
        )
        .format(&Rfc3339)
        .unwrap_or_else(|_| MAX.into())
}

#[allow(clippy::type_complexity)]
fn process_keys(
    public: PublicSlot,
    db_public: &mut PublicSlot,
    mut old_pairs: HashMap<PublicSlot, (Pair, String)>,
    old_publics_death_timestamps: Vec<(PublicSlot, Compact<Timestamp>)>,
) -> Result<(
    HashMap<PublicSlot, Pair>,
    Vec<(PublicSlot, Compact<Timestamp>)>,
)> {
    let mut checked_old_pairs = HashMap::with_capacity(old_pairs.len());
    let mut new_old_publics_death_timestamps =
        Vec::with_capacity(old_publics_death_timestamps.len());
    let mut restored_key_timestamp = None;
    let duplicate_message = |old_public| {
        tracing::debug!(
            "Detected a public key duplicate {:?} in the database.",
            encode_to_hex(old_public)
        );
    };

    for (old_public, timestamp) in old_publics_death_timestamps {
        match old_pairs.remove(&old_public) {
            Some((old_pair, old_seed)) => {
                tracing::info!(
                    "The public key {:?} in the database was matched with `{OLD_SEED}{old_seed}`.",
                    encode_to_hex(old_public)
                );

                checked_old_pairs.insert(old_public, old_pair);
                new_old_publics_death_timestamps.push((old_public, timestamp));
            }
            None if old_public == public => {
                if restored_key_timestamp.is_none() {
                    restored_key_timestamp = Some(timestamp.0);
                } else {
                    duplicate_message(old_public);
                }
            }
            None if checked_old_pairs.contains_key(&old_public) => {
                duplicate_message(old_public);
            }
            None => {
                anyhow::bail!(
                    "public key {:?} that was removed on {} has no matching seed in `{OLD_SEED}*`",
                    encode_to_hex(old_public),
                    format_timestamp(timestamp.0)
                );
            }
        }
    }

    if public != *db_public {
        if let Some(timestamp) = restored_key_timestamp {
            tracing::info!(
                "The current key {:?} will be restored from {:?} removed on {}.",
                encode_to_hex(&db_public),
                encode_to_hex(public),
                format_timestamp(timestamp)
            );
        } else {
            tracing::info!(
                "The current key {:?} will be changed to {:?}.",
                encode_to_hex(&db_public),
                encode_to_hex(public)
            );
        }

        let timestamp: Timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .context("time travel is not supported, check the system time correctness")?
            .as_millis()
            .try_into()
            .context("system time is too far in the future, check the system time correctness")?;

        new_old_publics_death_timestamps.push((public, timestamp.into()));

        *db_public = public;
    }

    for (_, old_seed_name) in old_pairs.into_values() {
        tracing::warn!(
            "`{OLD_SEED}{old_seed_name:?}` has no matching public keys and thus is ignored."
        );
    }

    Ok((checked_old_pairs, new_old_publics_death_timestamps))
}

fn process_db_chain(
    chains: &mut HashMap<Arc<String>, ConnectedChain>,
    name: String,
    properties: ChainProperties,
    db_chains: &mut Vec<(Rc<String>, ChainProperties)>,
    chain_hashes: &mut HashSet<ChainHash>,
    checked_chains: &mut CheckedChains,
) -> Result<()> {
    if !chain_hashes.insert(properties.hash) {
        tracing::debug!("Detected a chain hash duplicate {name:?} in the database.");

        return Ok(());
    }

    let checked_connected_chain = if let Some(connected_chain) = chains.remove(&name) {
        if connected_chain.genesis.0 != properties.genesis {
            anyhow::bail!(
                "chain {name:?} has different genesis hashes in the database & from an RPC server, check RPC server URLs for correctness"
            );
        }

        Some(connected_chain)
    } else {
        None
    };

    let name_rc = Rc::new(name);

    match checked_chains.entry(name_rc.clone()) {
        Entry::Occupied(_) => {
            tracing::debug!(
                "Detected a chain name duplicate {:?} in the database.",
                encode_to_hex(properties.hash)
            );

            return Ok(());
        }
        Entry::Vacant(entry) => entry.insert((checked_connected_chain, properties.hash)),
    };

    tracing::info!(
        "The {name_rc:?} chain stored in the database is assigned to the {:?} hash.",
        encode_to_hex(properties.hash)
    );

    db_chains.push((name_rc, properties));

    Ok(())
}

fn process_chain(
    name: Arc<String>,
    chain: ConnectedChain,
    chain_hashes: &mut HashSet<ChainHash>,
    checked_chains: &mut CheckedChains,
    db_chains: &mut Vec<(Rc<String>, ChainProperties)>,
) {
    let genesis = U256::from(chain.genesis.0);
    let mut chain_hash = genesis;
    let mut step = U256::one();

    loop {
        let hash = chain_hash.into();

        if chain_hashes.insert(hash) {
            tracing::info!(
                "A new {name:?} chain is assigned to the {:?} hash.",
                encode_to_hex(hash)
            );

            let name_rc = Rc::new(Arc::unwrap_or_clone(name));

            db_chains.push((
                name_rc.clone(),
                ChainProperties {
                    genesis: chain.genesis.0,
                    hash,
                },
            ));
            checked_chains.insert(name_rc.clone(), (Some(chain), hash));

            break;
        }

        tracing::debug!(
            "Failed to assign a new {name:?} chain to the {:?} hash. Probing the next slot...",
            encode_to_hex(hash)
        );

        chain_hash = chain_hash.overflowing_add(step).0;

        assert!(
            chain_hash == genesis,
            "database can't store more than `U256::MAX` chains"
        );

        step = step.overflowing_add(U256::one()).0;
    }
}

#[cfg(test)]
#[test]
fn daemon_info_encode_like() {
    const PUBLIC: [u8; 32] = [
        67, 249, 198, 209, 226, 235, 205, 138, 160, 184, 185, 173, 224, 86, 98, 220, 181, 239, 43,
        64, 209, 9, 155, 163, 147, 126, 239, 183, 186, 218, 102, 251,
    ];
    const NAME_1: &str = "Qdr9F9ATV9RXeC2nqIwM2EJfNVtdY6c5";
    const NAME_2: &str = "gDmEnXvK0URjeeAb1iYcOOJnvT7c95RF";
    const NAME_3: &str = "dVqeUeKul4Xh5Lc0KFcwpGOO0OALmUp6";
    const CP_1: ChainProperties = ChainProperties {
        genesis: [
            234, 239, 219, 118, 15, 192, 253, 27, 171, 102, 81, 242, 184, 239, 161, 231, 128, 48,
            128, 64, 226, 226, 19, 105, 49, 250, 31, 127, 109, 69, 208, 108,
        ],
        hash: [
            205, 62, 8, 46, 7, 136, 127, 133, 66, 158, 18, 97, 4, 209, 94, 173, 93, 187, 155, 148,
            118, 89, 0, 73, 182, 21, 115, 227, 120, 89, 177, 171,
        ],
    };
    const CP_2: ChainProperties = ChainProperties {
        genesis: [
            153, 143, 7, 243, 214, 21, 132, 214, 181, 219, 144, 47, 136, 243, 152, 25, 52, 230,
            252, 54, 83, 215, 200, 11, 83, 203, 82, 152, 147, 236, 150, 103,
        ],
        hash: [
            103, 145, 112, 79, 239, 125, 241, 3, 4, 152, 25, 20, 198, 8, 214, 209, 100, 191, 93,
            30, 34, 143, 230, 41, 34, 13, 38, 98, 16, 174, 221, 8,
        ],
    };
    const CP_3: ChainProperties = ChainProperties {
        genesis: [
            166, 66, 247, 222, 99, 184, 139, 207, 112, 52, 246, 68, 12, 34, 102, 225, 90, 162, 60,
            229, 202, 110, 63, 99, 47, 226, 168, 139, 187, 197, 89, 43,
        ],
        hash: [
            3, 12, 30, 46, 182, 250, 248, 209, 10, 211, 29, 233, 167, 147, 16, 152, 192, 168, 125,
            193, 28, 227, 205, 240, 62, 166, 22, 178, 64, 68, 105, 142,
        ],
    };
    const OLD_PUBLIC_1: [u8; 32] = [
        168, 126, 144, 105, 86, 250, 102, 55, 150, 0, 233, 0, 59, 23, 113, 193, 140, 130, 24, 213,
        235, 195, 209, 61, 130, 124, 216, 186, 225, 140, 28, 91,
    ];
    const OLD_PUBLIC_2: [u8; 32] = [
        201, 120, 253, 37, 68, 227, 70, 102, 149, 44, 233, 30, 65, 253, 231, 66, 45, 149, 82, 23,
        92, 236, 18, 249, 95, 71, 181, 20, 30, 210, 122, 145,
    ];
    const OLD_PUBLIC_3: [u8; 32] = [
        101, 212, 150, 240, 254, 252, 55, 80, 231, 226, 67, 228, 196, 168, 213, 225, 137, 87, 160,
        125, 137, 178, 247, 94, 99, 250, 44, 110, 160, 230, 226, 41,
    ];

    let chains = vec![
        (Rc::new(NAME_1.to_string()), CP_1),
        (Rc::new(NAME_2.to_string()), CP_2),
        (Rc::new(NAME_3.to_string()), CP_3),
    ];
    let old_publics_death_timestamps = vec![
        (OLD_PUBLIC_1, 4_802_314_881_109_433_217.into()),
        (OLD_PUBLIC_1, 4_802_314_881_094.into()),
        (OLD_PUBLIC_1, 48_081_109_433_217.into()),
    ];

    let daemon_info_ref = DaemonInfoRef {
        chains: chains.clone(),
        public: PUBLIC,
        old_publics_death_timestamps: old_publics_death_timestamps.clone(),
    }
    .encode();

    let daemon_info = DaemonInfo::decode(&mut daemon_info_ref.as_ref()).unwrap();

    assert_eq!(
        daemon_info.chains,
        chains
            .into_iter()
            .map(|chain| (Rc::unwrap_or_clone(chain.0), chain.1.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(daemon_info.public, PUBLIC);
    assert_eq!(
        daemon_info.old_publics_death_timestamps,
        old_publics_death_timestamps
    );
}

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
