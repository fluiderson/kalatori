//! The database module.
//!
//! We do not need concurrency here as this is our actual source of truth for legally binging
//! commercial offers and contracts, hence causality is a must. Care must be taken that no threads
//! are spawned here and all locking methods are called in sync functions without sharing lock
//! guards with the async code.

use crate::{
    arguments::OLD_SEED,
    chain::definitions::{Account, BlockHash, ConnectedChain, H256},
    error::DbError,
    signer::{KeyStore, Signer},
};
use ahash::{HashMap, HashMapExt, HashSet, HashSetExt, RandomState};
use codec::{Decode, Encode};
use indexmap::{
    map::{Entry, VacantEntry},
    IndexMap,
};
use names::{Generator, Name};
use redb::{
    backends::{FileBackend, InMemoryBackend},
    AccessGuard, Database as Redb, ReadableTable, Table, TableHandle, Value, WriteTransaction,
};
use ruint::aliases::U256;
use std::{
    borrow::Cow,
    fmt::{Display, Formatter, Result as FmtResult},
    fs::File,
    io::ErrorKind,
    path,
    sync::Arc,
    time::SystemTime,
};

pub mod definitions;

use definitions::{
    AccountsTable, ChainHash, ChainProperties, ChainTableTrait, DaemonInfo, HitListTable,
    InvoicesTable, KeysTable, Public, RootKey, RootTable, RootValue, TableTrait, TableTypes,
    Timestamp, Version,
};

pub const MODULE: &str = module_path!();
pub const DB_VERSION: Version = Version(0);

struct PathPrinter<'a>(&'a str);

impl Display for PathPrinter<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match path::absolute(self.0) {
            Ok(absolute) => absolute.display().fmt(f),
            Err(_) => self.0.fmt(f),
        }
    }
}

pub struct Database {
    db: Redb,
    instance: String,
    recipient: Account,
}

impl Database {
    #[allow(clippy::too_many_lines)]
    pub fn new(
        path_option: Option<Cow<'static, str>>,
        connected_chains: &IndexMap<Arc<str>, ConnectedChain, RandomState>,
        key_store: KeyStore,
        recipient: Account,
    ) -> Result<(Arc<Self>, Arc<Signer>), DbError> {
        let builder = Redb::builder();

        let (mut database, is_new) = if let Some(path) = path_option {
            match File::create_new(&*path) {
                Ok(file) => {
                    tracing::info!("Creating the database at {}.", PathPrinter(&path));

                    FileBackend::new(file)
                        .and_then(|backend| builder.create_with_backend(backend))
                        .map(|db| (db, true))
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    tracing::info!("Opening the database at {}.", PathPrinter(&path));

                    builder.open(&*path).map(|db| (db, false))
                }
                Err(error) => Err(error.into()),
            }
        } else {
            tracing::warn!(
                "The in-memory backend for the database is selected. All saved data will be deleted after the shutdown!"
            );

            builder
                .create_with_backend(InMemoryBackend::new())
                .map(|db| (db, true))
        }?;

        let tx = database.begin_write()?;
        let mut root = open_table::<RootTable>(&tx)?;
        let mut chain_hashes = HashSet::with_capacity(connected_chains.len());
        let public = key_store.public();

        let instance;
        let daemon_info;
        let signer;

        if is_new {
            signer = key_store.into_signer::<false>(HashMap::new());

            let mut new_db_chains = Vec::with_capacity(connected_chains.len());

            for (name, connected_chain) in connected_chains {
                process_new_chain(
                    &mut chain_hashes,
                    (name.as_ref().to_owned(), &mut new_db_chains),
                    connected_chain.genesis,
                );
            }

            instance = Generator::with_naming(Name::Numbered).next().unwrap();
            daemon_info = DaemonInfo {
                chains: new_db_chains,
                public,
                old_publics_death_timestamps: vec![],
                instance: instance.clone().into_bytes(),
            };

            insert_slot::<RootTable>(
                &mut root,
                &RootKey::DbVersion,
                &RootValue(&Version::as_bytes(&DB_VERSION)),
            )?;
        } else {
            let Some(encoded_db_version) = get_slot::<RootTable>(&root, &RootKey::DbVersion)?
            else {
                return Err(DbError::NoVersion);
            };

            let db_version = Version::from_bytes(encoded_db_version.value().0);

            if db_version != DB_VERSION {
                return Err(DbError::UnexpectedVersion(db_version));
            }

            let Some(encoded_daemon_info) = get_slot::<RootTable>(&root, &RootKey::DaemonInfo)?
            else {
                return Err(DbError::NoDaemonInfo);
            };

            let DaemonInfo {
                chains: db_chains,
                public: mut db_public,
                old_publics_death_timestamps,
                instance: db_instance,
            } = DaemonInfo::decode(&mut encoded_daemon_info.value().0)?;

            let mut new_db_chains = IndexMap::with_capacity_and_hasher(
                db_chains.len() + connected_chains.len(),
                RandomState::new(),
            );

            for (name, properties) in db_chains {
                process_db_chain(
                    name,
                    properties,
                    &mut chain_hashes,
                    &mut new_db_chains,
                    connected_chains,
                )?;
            }

            for (name, connected_chain) in connected_chains {
                if let Entry::Vacant(entry) = new_db_chains.entry(name.as_ref().to_owned()) {
                    process_new_chain(&mut chain_hashes, entry, connected_chain.genesis);
                }
            }

            let tables = Tables::get(&tx, &chain_hashes)?;
            let (new_old_publics_death_timestamps, prepared_signer) = process_keys(
                &public,
                db_public,
                key_store,
                old_publics_death_timestamps,
                tables.keys,
            )?;

            signer = prepared_signer;

            let mut new_db_chains_vec = Vec::with_capacity(new_db_chains.len());

            new_db_chains_vec.extend(new_db_chains);

            instance = String::from_utf8(db_instance)
                .expect("should be a valid UTF-8 text since we encoded it as one");
            daemon_info = DaemonInfo {
                chains: new_db_chains_vec,
                public,
                old_publics_death_timestamps: new_old_publics_death_timestamps,
                instance: instance.clone().into_bytes(),
            };
        }

        insert_slot::<RootTable>(
            &mut root,
            &RootKey::DaemonInfo,
            &RootValue(&daemon_info.encode()),
        )?;
        drop(root);

        tx.commit()?;

        if database.compact()? {
            tracing::debug!("The database has been compacted.");
        } else {
            tracing::debug!("The database doesn't need to be compacted.");
        }

        Ok((
            Arc::new(Self {
                db: database,
                instance,
                recipient,
            }),
            Arc::new(signer),
        ))
    }
}

struct Tables<'a> {
    keys: Option<Table<'a, <KeysTable as TableTypes>::Key, <KeysTable as TableTypes>::Value>>,
    invoices:
        Option<Table<'a, <InvoicesTable as TableTypes>::Key, <InvoicesTable as TableTypes>::Value>>,

    accounts: HashMap<
        ChainHash,
        Table<'a, <AccountsTable as TableTypes>::Key, <AccountsTable as TableTypes>::Value>,
    >,
    hit_list: HashMap<
        ChainHash,
        Table<'a, <HitListTable as TableTypes>::Key, <HitListTable as TableTypes>::Value>,
    >,
}

impl<'a> Tables<'a> {
    /// Collects all tables in the database & purges unknown ones (e.g. those left after a
    /// migration).
    fn get(
        tx: &'a WriteTransaction,
        chain_hashes: &HashSet<ChainHash>,
    ) -> Result<Tables<'a>, DbError> {
        let mut keys = None;
        let mut invoices = None;
        let mut accounts = HashMap::new();
        let mut hit_list = HashMap::new();

        for table in tx.list_tables().map_err(DbError::TableList)? {
            match table.name() {
                RootTable::NAME => {}
                KeysTable::NAME => keys = Some(open_table::<KeysTable>(tx)?),
                InvoicesTable::NAME => invoices = Some(open_table::<InvoicesTable>(tx)?),
                other_name => {
                    if open_chain_tables(
                        tx,
                        chain_hashes,
                        other_name,
                        &mut accounts,
                        &mut hit_list,
                    )? {
                        tracing::debug!(
                            "Detected an unknown table {other_name:?}, it'll be purged."
                        );

                        tx.delete_table(table).map_err(DbError::DeleteTable)?;
                    }
                }
            }
        }

        Ok(Self {
            keys,
            invoices,
            accounts,
            hit_list,
        })
    }
}

fn open_chain_tables<'a>(
    tx: &'a WriteTransaction,
    chain_hashes: &HashSet<ChainHash>,
    other_name: &str,
    accounts: &mut HashMap<
        ChainHash,
        Table<'a, <AccountsTable as TableTypes>::Key, <AccountsTable as TableTypes>::Value>,
    >,
    hit_list: &mut HashMap<
        ChainHash,
        Table<'a, <HitListTable as TableTypes>::Key, <HitListTable as TableTypes>::Value>,
    >,
) -> Result<bool, DbError> {
    let Some(hash_start) = other_name.len().checked_sub(H256::HEX_LENGTH) else {
        return Ok(true);
    };

    // TODO: https://github.com/rust-lang/rust/issues/119128
    let Some((stripped, maybe_hash)) = other_name.is_char_boundary(hash_start).then(|| {
        (
            other_name.get(0..hash_start).unwrap(),
            other_name.get(hash_start..other_name.len()).unwrap(),
        )
    }) else {
        return Ok(true);
    };

    match stripped {
        AccountsTable::PREFIX => AccountsTable::try_open(maybe_hash, chain_hashes, tx, accounts),
        HitListTable::PREFIX => HitListTable::try_open(maybe_hash, chain_hashes, tx, hit_list),
        _ => Ok(true),
    }
}

fn open_table<T: TableTrait>(
    tx: &WriteTransaction,
) -> Result<Table<'_, T::Key, T::Value>, DbError> {
    tx.open_table(T::DEFINITION).map_err(DbError::OpenTable)
}

fn insert_slot<T: TableTypes>(
    table: &mut Table<'_, T::Key, T::Value>,
    key: &<T::Key as Value>::SelfType<'_>,
    value: &<T::Value as Value>::SelfType<'_>,
) -> Result<(), DbError> {
    table
        .insert(key, value)
        .map(|_| ())
        .map_err(DbError::Insert)
}

fn get_slot<'a, T: TableTypes>(
    table: &'a impl ReadableTable<T::Key, T::Value>,
    key: &<T::Key as Value>::SelfType<'_>,
) -> Result<Option<AccessGuard<'a, T::Value>>, DbError> {
    table.get(key).map_err(DbError::Get)
}

trait ProcessNewChainHelper {
    fn name(&self) -> &str;
    fn add_to_db_chains(self, properties: ChainProperties);
}

impl ProcessNewChainHelper for VacantEntry<'_, String, ChainProperties> {
    fn name(&self) -> &str {
        self.key()
    }

    fn add_to_db_chains(self, properties: ChainProperties) {
        self.insert(properties);
    }
}

impl ProcessNewChainHelper for (String, &mut Vec<(String, ChainProperties)>) {
    fn name(&self) -> &str {
        &self.0
    }

    fn add_to_db_chains(self, properties: ChainProperties) {
        self.1.push((self.0, properties));
    }
}

fn process_new_chain(
    chain_hashes: &mut HashSet<ChainHash>,
    helper: impl ProcessNewChainHelper,
    genesis: BlockHash,
) {
    let mut chain_hash = genesis.0.into();
    let one = U256::try_from(1u64).unwrap();
    let mut step = one;

    loop {
        if chain_hashes.insert(chain_hash) {
            tracing::debug!(
                "The new {:?} chain is assigned to the {:#} hash.",
                helper.name(),
                H256::from(chain_hash)
            );

            helper.add_to_db_chains(ChainProperties {
                genesis: genesis.into(),
                hash: chain_hash,
            });

            break;
        }

        tracing::debug!(
            "Failed to assign the new {:?} chain to the {:#} hash. Probing the next slot...",
            helper.name(),
            H256::from(chain_hash)
        );

        chain_hash = H256(U256::from_be_bytes(chain_hash.0).overflowing_add(step).0).into();
        step = step
            .checked_add(one)
            .expect("database can't store more than `U256::MAX` chains");
    }
}

fn process_db_chain(
    name: String,
    properties: ChainProperties,
    chain_hashes: &mut HashSet<ChainHash>,
    new_db_chains: &mut IndexMap<String, ChainProperties, RandomState>,
    connected_chains: &IndexMap<Arc<str>, ConnectedChain, RandomState>,
) -> Result<(), DbError> {
    if !chain_hashes.insert(properties.hash) {
        tracing::debug!(
            "Found the {name:?} chain with the hash duplicate {:#} in the database.",
            H256::from(properties.hash)
        );

        return Ok(());
    }

    let entry = match new_db_chains.entry(name) {
        Entry::Occupied(entry) => {
            tracing::debug!(
                "Found 2 chains with same name ({:?}) in the database.",
                entry.key()
            );

            return Ok(());
        }
        Entry::Vacant(entry) => entry,
    };

    tracing::debug!(name = entry.key(), properties = ?properties);

    if let Some(connected_chain) = connected_chains.get(&**entry.key()) {
        let given = connected_chain.genesis.into();

        if given != properties.genesis {
            return Err(DbError::GenesisMismatch {
                chain: entry.into_key(),
                expected: properties.genesis,
                given,
            });
        }

        tracing::debug!(
            "The {:?} chain stored in the database is assigned to the {:?} hash.",
            entry.key(),
            H256::from(properties.hash),
        );
    } else {
        tracing::warn!(
            "The {:?} chain exists in the database but doesn't present in the config.",
            entry.key(),
        );
    }

    entry.insert(properties);

    Ok(())
}

fn process_keys(
    public: &Public,
    db_public: Public,
    mut key_store: KeyStore,
    old_publics_death_timestamps: Vec<(Public, Timestamp)>,
    keys: Option<Table<'_, <KeysTable as TableTypes>::Key, <KeysTable as TableTypes>::Value>>,
) -> Result<(Vec<(Public, Timestamp)>, Signer), DbError> {
    // Since the current key can be changed, this array should've a capacity of one more element.
    let mut new_old_publics_death_timestamps =
        Vec::with_capacity(old_publics_death_timestamps.len() + 1);
    let mut filtered_old_pairs = HashMap::with_capacity(key_store.old_pairs_len());
    let mut restored_key_timestamp = None;
    let print_about_duplicate = |old_public| {
        tracing::debug!(
            "Found a public key duplicate {:#} in the database.",
            H256::from(old_public)
        );
    };

    for (old_public, timestamp) in old_publics_death_timestamps {
        match key_store.remove(public) {
            Some((entropy, name)) => {
                tracing::info!(
                    "The public key {:#} in the database was matched with `{OLD_SEED}{name}`.",
                    H256::from(old_public)
                );

                filtered_old_pairs.insert(old_public, (entropy, name));
                new_old_publics_death_timestamps.push((old_public, timestamp));
            }
            None if old_public == *public => {
                if restored_key_timestamp.is_none() {
                    restored_key_timestamp = Some(timestamp);
                } else {
                    print_about_duplicate(old_public);
                }
            }
            None if filtered_old_pairs.contains_key(&old_public) => {
                print_about_duplicate(old_public);
            }
            None => {
                return Err(DbError::OldKeyNotFound {
                    key: old_public,
                    removed: timestamp,
                })
            }
        }
    }

    if *public != db_public {
        if let Some(timestamp) = restored_key_timestamp {
            tracing::info!(
                "The current key {:#} will be changed to {:#} removed on {}.",
                H256::from(db_public),
                H256::from(*public),
                timestamp,
            );
        } else {
            tracing::info!(
                "The current key {:#} will be changed to {:#}.",
                H256::from(db_public),
                H256::from(*public),
            );
        }

        let is_db_public_in_circulation = keys.map_or(Ok(false), |table| {
            get_slot::<KeysTable>(&table, &db_public).map(|slot_option| slot_option.is_some())
        })?;

        if is_db_public_in_circulation {
            key_store
                .remove(&db_public)
                .ok_or(DbError::CurrentKeyNotFound(db_public))?;

            let timestamp = Timestamp(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_err(|_| DbError::AbnormalSystemTime)?
                    .as_millis()
                    .try_into()
                    .map_err(|_| DbError::AbnormalSystemTime)?,
            );

            new_old_publics_death_timestamps.push((db_public, timestamp));
        } else {
            tracing::info!(
                "The current key has no accounts associated with it and hence will be deleted immediately."
            );
        }
    }

    Ok((
        new_old_publics_death_timestamps,
        key_store.into_signer::<true>(filtered_old_pairs),
    ))
}
