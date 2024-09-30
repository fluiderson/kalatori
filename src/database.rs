//! The database module.
//!
//! We do not need concurrency here as this is our actual source of truth for legally binging
//! commercial offers and contracts, hence causality is a must. A care must be taken that no threads
//! are spawned here and all locking methods are called in sync functions without sharing lock
//! guards with the async code.

use crate::{
    arguments::OLD_SEED,
    chain_wip::definitions::{BlockHash, ConnectedChain, H256, H64},
    error::DbError,
    signer::{KeyStore, Signer},
    utils::PathDisplay,
};
use ahash::{HashMap, HashMapExt, HashSet, HashSetExt, RandomState};
use arguments::CreatedOrder;
use codec::{Decode, Encode};
use indexmap::{
    map::{Entry, VacantEntry},
    IndexMap,
};
use names::{Generator, Name};
use redb::{
    backends::{FileBackend, InMemoryBackend},
    AccessGuard, Database as Redb, ReadOnlyTable, ReadTransaction, ReadableTable, Table,
    TableHandle, Value, WriteTransaction,
};
use std::{
    array, borrow::Cow, ffi::OsStr, fs::File, io::ErrorKind, num::NonZeroU64 as StdNonZeroU64,
    sync::Arc,
};

pub mod definitions;

use definitions::{
    ChainHash, ChainProperties, DaemonInfo, KeysTable, NonZeroU64, OrderId, OrderInfo,
    OrdersPerChainTable, OrdersTable, PaymentStatus, Public, RootKey, RootTable, RootValue,
    TableRead, TableTrait, TableTypes, TableWrite, Timestamp, Version,
};

pub const MODULE: &str = module_path!();
pub const DB_VERSION: Version = Version(0);

pub struct Database {
    db: Redb,
    instance: String,
}

impl Database {
    #[allow(clippy::too_many_lines)]
    pub fn new(
        path_option: Option<Cow<'static, OsStr>>,
        connected_chains: &IndexMap<Arc<str>, ConnectedChain, RandomState>,
        key_store: KeyStore,
    ) -> Result<(Arc<Self>, Arc<Signer>), DbError> {
        let builder = Redb::builder();

        let (mut database, is_new) = if let Some(path) = path_option {
            match File::create_new(&path) {
                Ok(file) => {
                    tracing::info!("Creating the database at {}.", PathDisplay(path));

                    FileBackend::new(file)
                        .and_then(|backend| builder.create_with_backend(backend))
                        .map(|db| (db, true))
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    tracing::info!("Opening the database at {}.", PathDisplay(&path));

                    builder.open(path).map(|db| (db, false))
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

        let tx = TxWrite::new(&database)?.0;
        let mut root = RootTable::open(&tx)?;
        let mut chain_hashes = HashSet::with_capacity(connected_chains.len());
        let public = key_store.public();

        let instance;
        let daemon_info;
        let signer;

        if is_new {
            signer = key_store.into_signer::<true>(HashMap::new());

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

            root.insert_slot(
                RootKey::DbVersion,
                RootValue(&Version::as_bytes(&DB_VERSION)),
            )?;
        } else {
            let Some(encoded_db_version) = root.get_slot(RootKey::DbVersion)? else {
                return Err(DbError::NoVersion);
            };

            let db_version = Version::from_bytes(encoded_db_version.value().0);

            if db_version != DB_VERSION {
                return Err(DbError::UnexpectedVersion(db_version));
            }

            let Some(encoded_daemon_info) = root.get_slot(RootKey::DaemonInfo)? else {
                return Err(DbError::NoDaemonInfo);
            };

            let DaemonInfo {
                chains: db_chains,
                public: db_public,
                old_publics_death_timestamps,
                instance: db_instance,
            } = DaemonInfo::decode(&mut encoded_daemon_info.value().0)?;

            #[expect(clippy::arithmetic_side_effects)]
            let mut new_db_chains = IndexMap::with_capacity_and_hasher(
                db_chains.len() + connected_chains.len(),
                RandomState::new(),
            );
            let mut orders_per_chain = OrdersPerChainTable::open(&tx)?;

            for (name, properties) in db_chains {
                process_db_chain(
                    name,
                    properties,
                    &mut chain_hashes,
                    &mut new_db_chains,
                    connected_chains,
                    &mut orders_per_chain,
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

        root.insert_slot(&RootKey::DaemonInfo, &RootValue(&daemon_info.encode()))?;

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
            }),
            Arc::new(signer),
        ))
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    pub fn read(&self) -> Result<TxRead, DbError> {
        self.db.begin_read().map_err(DbError::TxRead).map(TxRead)
    }

    pub fn write<T>(&self, f: impl FnOnce(&TxWrite) -> Result<T, DbError>) -> Result<T, DbError> {
        let tx = TxWrite::new(&self.db)?;

        let t = f(&tx)?;

        tx.0.commit()?;

        Ok(t)
    }
}

pub struct TxRead(ReadTransaction);

impl TxRead {
    pub fn orders(&self) -> Result<Option<OrdersRead>, DbError> {
        OrdersTable::open_ro(&self.0).map(|some| some.map(OrdersRead))
    }
}

pub struct OrdersRead(
    ReadOnlyTable<<OrdersTable as TableTypes>::Key, <OrdersTable as TableTypes>::Value>,
);

impl OrdersRead {
    pub fn read(
        &self,
        key: impl AsRef<str>,
    ) -> Result<Option<<OrdersTable as TableTypes>::Value>, DbError> {
        self.0
            .get_slot(OrderId::from(key.as_ref()))
            .map(|some| some.map(|ag| ag.value()))
    }

    pub fn active(
        &self,
    ) -> Result<
        impl Iterator<
            Item = Result<
                (
                    AccessGuard<'_, <OrdersTable as TableTypes>::Key>,
                    <OrdersTable as TableTypes>::Value,
                ),
                DbError,
            >,
        >,
        DbError,
    > {
        self.0.iter().map_err(DbError::Range).map(|range| {
            range.filter_map(|result| match result {
                Ok(r) => {
                    let order = r.1.value();

                    (order.payment_status == PaymentStatus::Pending).then(|| Ok((r.0, order)))
                }
                Err(e) => Some(Err(DbError::RangeIter(e))),
            })
        })
    }
}

pub struct TxWrite(WriteTransaction);

impl TxWrite {
    fn new(db: &Redb) -> Result<Self, DbError> {
        db.begin_write().map(TxWrite).map_err(DbError::TxWrite)
    }

    pub fn orders(&self) -> Result<OrdersWrite<'_>, DbError> {
        OrdersTable::open(&self.0).map(OrdersWrite)
    }

    pub fn orders_per_chain(&self) -> Result<OrdersPerChainWrite<'_>, DbError> {
        OrdersPerChainTable::open(&self.0).map(OrdersPerChainWrite)
    }

    pub fn create_order(
        mut orders: OrdersWrite<'_>,
        mut orders_per_chain: OrdersPerChainWrite<'_>,
        chain: ChainHash,
        key: impl AsRef<str>,
        new_order: OrderInfo,
    ) -> Result<CreatedOrder, DbError> {
        let order_id = OrderId::from(key.as_ref());
        let order_option = orders.0.get_slot(order_id)?.map(|ag| ag.value());

        Ok(if let Some(order) = order_option {
            if order.payment_status == PaymentStatus::Pending {
                CreatedOrder::Modified(new_order)
            } else {
                CreatedOrder::Unchanged(order)
            }
        } else {
            orders.0.insert_slot(order_id, &new_order)?;

            let orders_amount = orders_per_chain
                .0
                .get_slot(chain)?
                .map(|ag| {
                    ag.value()
                        .0
                        .checked_add(1)
                        .expect("database can't store more than `u64::MAX` orders per chain")
                })
                .unwrap_or(StdNonZeroU64::MIN);

            orders_per_chain
                .0
                .insert_slot(chain, NonZeroU64(orders_amount))?;

            CreatedOrder::Unchanged(new_order)
        })
    }
}

pub struct OrdersWrite<'a>(
    Table<'a, <OrdersTable as TableTypes>::Key, <OrdersTable as TableTypes>::Value>,
);

impl OrdersWrite<'_> {
    pub fn mark_paid(
        &mut self,
        key: impl AsRef<str>,
    ) -> Result<<OrdersTable as TableTypes>::Value, DbError> {
        let key_as_ref = key.as_ref();

        let Some(mut order) = self
            .0
            .get_slot(OrderId::from(key_as_ref))?
            .map(|ag| ag.value())
        else {
            return Err(DbError::OrderNotFound(key_as_ref.into()));
        };

        if order.payment_status != PaymentStatus::Pending {
            return Err(DbError::OrderAlreadyPaid(key_as_ref.into()));
        }

        order.payment_status = PaymentStatus::Paid;

        self.0.insert_slot(OrderId::from(key_as_ref), &order)?;

        Ok(order)
    }
}

pub struct OrdersPerChainWrite<'a>(
    Table<'a, <OrdersPerChainTable as TableTypes>::Key, <OrdersPerChainTable as TableTypes>::Value>,
);

struct Tables<'a> {
    keys: Option<Table<'a, <KeysTable as TableTypes>::Key, <KeysTable as TableTypes>::Value>>,
    orders: Option<Table<'a, <OrdersTable as TableTypes>::Key, <OrdersTable as TableTypes>::Value>>,
}

impl<'a> Tables<'a> {
    /// Collects all tables in the database & purges unknown ones (e.g. those left after a
    /// migration).
    fn get(
        tx: &'a WriteTransaction,
        chain_hashes: &HashSet<ChainHash>,
    ) -> Result<Tables<'a>, DbError> {
        let mut keys = None;
        let mut orders = None;

        for table in tx.list_tables().map_err(DbError::TableList)? {
            match table.name() {
                RootTable::NAME | OrdersPerChainTable::NAME => {}
                KeysTable::NAME => keys = Some(KeysTable::open(tx)?),
                OrdersTable::NAME => orders = Some(OrdersTable::open(tx)?),
                other_name => {
                    if open_chain_tables(tx, chain_hashes, other_name)? {
                        tracing::debug!(
                            "Detected an unknown table {other_name:?}, it'll be purged."
                        );

                        tx.delete_table(table).map_err(DbError::DeleteTable)?;
                    }
                }
            }
        }

        Ok(Self { keys, orders })
    }
}

#[expect(clippy::unnecessary_wraps, unused)]
fn open_chain_tables(
    tx: &WriteTransaction,
    chain_hashes: &HashSet<ChainHash>,
    other_name: &str,
) -> Result<bool, DbError> {
    let Some(hash_start) = other_name.len().checked_sub(H64::HEX_LENGTH) else {
        return Ok(true);
    };

    let Some((stripped, maybe_hash)) = other_name.split_at_checked(hash_start) else {
        return Ok(true);
    };

    #[expect(clippy::match_single_binding)]
    match stripped {
        // TODO: Arms with chain tables.
        _ => Ok(true),
    }
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

fn strip_array<T, const N1: usize, const N2: usize>(array: [T; N1]) -> [T; N2] {
    let mut iterator = array.into_iter();

    array::from_fn(|_| {
        iterator.next().expect(
            "number of elements in the returned array should be equal or less than in the given \
            array",
        )
    })
}

fn process_new_chain(
    chain_hashes: &mut HashSet<ChainHash>,
    helper: impl ProcessNewChainHelper,
    genesis: BlockHash,
) {
    let mut chain_hash = ChainHash(strip_array(genesis.0.to_be_bytes()));
    let mut step = 1;

    loop {
        if chain_hashes.insert(chain_hash) {
            tracing::debug!(
                "The new {:?} chain is assigned to the {:#} hash.",
                helper.name(),
                H64::from(chain_hash)
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
            H64::from(chain_hash)
        );

        chain_hash = H64(H64::from_be_bytes(chain_hash.0).0.overflowing_add(step).0).into();
        step = step
            .checked_add(1)
            .expect("database can't store more than `u64::MAX` chains");
    }
}

fn process_db_chain(
    name: String,
    properties: ChainProperties,
    chain_hashes: &mut HashSet<ChainHash>,
    new_db_chains: &mut IndexMap<String, ChainProperties, RandomState>,
    connected_chains: &IndexMap<Arc<str>, ConnectedChain, RandomState>,
    orders_per_chain: &mut Table<
        '_,
        <OrdersPerChainTable as TableTypes>::Key,
        <OrdersPerChainTable as TableTypes>::Value,
    >,
) -> Result<(), DbError> {
    if !chain_hashes.insert(properties.hash) {
        tracing::debug!(
            "Found the {name:?} chain with the hash duplicate {:#} in the database.",
            H64::from(properties.hash)
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
            H64::from(properties.hash),
        );
    } else if orders_per_chain.get_slot(properties.hash)?.is_some() {
        tracing::warn!(
            "The {:?} chain exists in the database but isn't present in the config.",
            entry.key(),
        );
    } else {
        tracing::warn!(
            "The {:?} chain exists in the database but isn't present in the config and has no \
            orders. It will be deleted.",
            entry.key()
        );

        return Ok(());
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
    #[expect(clippy::arithmetic_side_effects)]
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
            table
                .get_slot(&db_public)
                .map(|slot_option| slot_option.is_some())
        })?;

        if is_db_public_in_circulation {
            key_store
                .remove(&db_public)
                .ok_or(DbError::CurrentKeyNotFound(db_public))?;
            new_old_publics_death_timestamps.push((db_public, Timestamp::now()?));
        } else {
            tracing::info!(
                "The current key has no accounts associated with it and hence will be deleted immediately."
            );
        }
    }

    Ok((
        new_old_publics_death_timestamps,
        key_store.into_signer::<false>(filtered_old_pairs),
    ))
}

pub mod arguments {
    use super::definitions::OrderInfo;

    pub enum CreatedOrder {
        New(OrderInfo),
        Modified(OrderInfo),
        Unchanged(OrderInfo),
    }
}
