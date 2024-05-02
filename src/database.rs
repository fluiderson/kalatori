use crate::{
    rpc::{ConnectedChain, InnerConnectedChain},
    AccountId, AssetId, Timestamp, Version, DB_VERSION, OLD_SEED,
};
use anyhow::{Context, Result};
use redb::{
    backends::{FileBackend, InMemoryBackend},
    AccessGuard, Database, Key, ReadOnlyTable, ReadTransaction, Table, TableDefinition, TableError,
    TableHandle, Value,
};
use std::{
    borrow::Cow,
    collections::{
        hash_map::{Entry, VacantEntry},
        HashMap,
    },
    fs::File,
    io::ErrorKind,
    sync::Arc,
    time::{Duration, SystemTime},
};
use subxt::ext::{
    codec::{Compact, Decode, Encode},
    sp_core::{sr25519::Pair, Pair as _, U256},
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

mod v1;

use v1::{ChainHash, ChainProperties, DaemonInfo, DAEMON_INFO, DB_VERSION_KEY, KEYS, ROOT};

pub use v1::PublicSlot;

pub const MODULE: &str = module_path!();

struct MappedCurrency {
    chain: ChainHash,
    asset: Option<AssetId>,
}

struct ChainInfo {
    name: String,
    rpc: String,
}

pub struct StateParameters {
    pub path_option: Option<Cow<'static, str>>,
    pub debug: Option<bool>,
    pub recipient: AccountId,
    pub remark: Option<String>,
    pub pair: Pair,
    pub old_pairs: HashMap<PublicSlot, (Pair, String)>,
    pub account_lifetime: Timestamp,
    pub chains: HashMap<String, ConnectedChain>,
    pub currencies: HashMap<Arc<String>, Option<AssetId>>,
}

pub struct State {
    recipient: AccountId,
    pair: Pair,
    old_pairs: HashMap<PublicSlot, Pair>,
    account_lifetime: Timestamp,
    debug: Option<bool>,
    remark: Option<String>,
    chains: HashMap<ChainHash, ChainInfo>,
    currencies: HashMap<String, MappedCurrency>,
}

impl State {
    #[allow(clippy::too_many_lines, clippy::type_complexity)]
    pub fn initialize(
        StateParameters {
            path_option,
            debug,
            recipient,
            remark,
            pair,
            old_pairs,
            account_lifetime,
            mut chains,
            mut currencies,
        }: StateParameters,
    ) -> Result<(Arc<Self>, Vec<(ChainHash, InnerConnectedChain)>)> {
        let builder = Database::builder();

        let (mut database, is_new) = if let Some(path) = path_option {
            tracing::info!("Creating/Opening the database at {path}.");

            match File::create_new(&*path) {
                Ok(file) => FileBackend::new(file)
                    .and_then(|backend| builder.create_with_backend(backend))
                    .map(|db| (db, true)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => builder
                    .create(&*path)
                    .map(|db| (db, false)),
                Err(error) => Err(error.into())
            }
        } else {
            tracing::warn!(
                "The in-memory backend for the database is selected. All saved data will be deleted after the shutdown!"
            );

            builder.create_with_backend(InMemoryBackend::new()).map(|db| (db, true))
        }.context("failed to create/open the database")?;

        let write_tx = database
            .begin_write()
            .context("failed to begin a read transaction")?;
        let mut root = write_tx.open_table(ROOT).with_context(|| {
            format!(
                "failed to open the {:?} table in a write transaction",
                ROOT.name()
            )
        })?;

        let read_tx = database
            .begin_read()
            .context("failed to begin a read transaction")?;
        let ro_root_option = open_ro_table(&read_tx, ROOT)?;
        let ro_root_slots = ro_root_option.as_ref().map_or(Ok(None), |ro_root| {
            get_slot(ro_root, DB_VERSION_KEY).and_then(|db_version| {
                get_slot(ro_root, DAEMON_INFO).map(|daemon_info| db_version.zip(daemon_info))
            })
        })?;

        let public = pair.public().0;
        let mut checked_chains = Vec::with_capacity(chains.len());
        let mut mapped_chains = HashMap::with_capacity(chains.len());
        let mut mapped_currencies = HashMap::with_capacity(currencies.len());

        let daemon_info;
        let mapped_old_pairs;

        if let Some((ref encoded_db_version, ref encoded_daemon_info)) = ro_root_slots {
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

            let (mapped_old_pairs_shadow, new_old_publics_death_timestamps) = process_keys(
                public,
                &mut db_public,
                old_pairs,
                old_publics_death_timestamps,
                &read_tx,
            )?;

            let mut new_db_chains =
                Vec::with_capacity(db_chains.len().saturating_add(chains.len()));

            for (name, properties) in db_chains {
                process_db_chain(
                    name,
                    &properties,
                    &mut chains,
                    ProcessChainParameters {
                        currencies: &mut currencies,
                        mapped_currencies: &mut mapped_currencies,
                        mapped_chains: &mut mapped_chains,
                        checked_chains: &mut checked_chains,
                        new_db_chains: &mut new_db_chains,
                    },
                )?;
            }

            for (name, chain) in chains {
                process_chain(
                    name,
                    chain,
                    ProcessChainParameters {
                        currencies: &mut currencies,
                        mapped_currencies: &mut mapped_currencies,
                        mapped_chains: &mut mapped_chains,
                        checked_chains: &mut checked_chains,
                        new_db_chains: &mut new_db_chains,
                    },
                );
            }

            mapped_old_pairs = mapped_old_pairs_shadow;
            daemon_info = DaemonInfo {
                chains: new_db_chains,
                public,
                old_publics_death_timestamps: new_old_publics_death_timestamps,
            }
            .encode();
        } else {
            if !is_new {
                anyhow::bail!(
                    "existing database doesn't contain {DB_VERSION_KEY:?} and/or {DAEMON_INFO:?}, maybe it was created by another program"
                );
            }

            if !old_pairs.is_empty() {
                tracing::warn!(
                    "The daemon has no existing database, so all `{OLD_SEED}*` are ignored."
                );
            }

            let mut new_db_chains = Vec::with_capacity(chains.len());

            for (name, chain) in chains {
                process_chain(
                    name,
                    chain,
                    ProcessChainParameters {
                        currencies: &mut currencies,
                        mapped_currencies: &mut mapped_currencies,
                        mapped_chains: &mut mapped_chains,
                        checked_chains: &mut checked_chains,
                        new_db_chains: &mut new_db_chains,
                    },
                );
            }

            mapped_old_pairs = HashMap::new();
            daemon_info = DaemonInfo {
                chains: new_db_chains,
                public,
                old_publics_death_timestamps: vec![],
            }
            .encode();

            insert_slot(&mut root, DB_VERSION_KEY, &DB_VERSION.encode())?;
        }

        insert_slot(&mut root, DAEMON_INFO, &daemon_info)?;
        drop(ro_root_slots);
        drop((root, ro_root_option, read_tx));

        write_tx
            .commit()
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
                recipient,
                pair,
                account_lifetime,
                debug,
                remark,
                old_pairs: mapped_old_pairs,
                chains: mapped_chains,
                currencies: mapped_currencies,
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

    // pub fn write(&self) -> Result<WriteTransaction<'_>> {
    //     // self
    //     //     .begin_write()
    //     //     .map(WriteTransaction)
    //     //     .context("failed to begin a write transaction for the database")
    //     todo!()
    // }

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

// pub struct WriteTransaction(redb::WriteTransaction);

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

fn get_slot<'a>(
    table: &'a ReadOnlyTable<&str, &[u8]>,
    key: &str,
) -> Result<Option<AccessGuard<'a, &'static [u8]>>> {
    table
        .get(key)
        .with_context(|| format!("failed to get the {key:?} slot"))
}

fn insert_slot(table: &mut Table<'_, &str, &[u8]>, key: &str, value: &[u8]) -> Result<()> {
    table
        .insert(key, value)
        .map(|_| ())
        .with_context(|| format!("failed to insert the {key:?} slot"))
}

fn decode_slot<T: Decode>(slot: &AccessGuard<'_, &[u8]>, key: &str) -> Result<T> {
    T::decode(&mut slot.value()).with_context(|| format!("failed to decode the {key:?} slot"))
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
    tx: &ReadTransaction,
) -> Result<(
    HashMap<PublicSlot, Pair>,
    Vec<(PublicSlot, Compact<Timestamp>)>,
)> {
    let mut mapped_old_pairs = HashMap::with_capacity(old_pairs.len());
    // Since the current key can be changed, this array should've a capacity of one more element.
    let mut new_old_publics_death_timestamps =
        Vec::with_capacity(old_publics_death_timestamps.len().saturating_add(1));
    let mut restored_key_timestamp = None;
    let duplicate_message = |old_public| {
        tracing::debug!(
            "Detected a public key duplicate {:?} in the database.",
            crate::encode_to_hex(old_public)
        );
    };

    for (old_public, timestamp) in old_publics_death_timestamps {
        match old_pairs.remove(&old_public) {
            Some((old_pair, old_seed)) => {
                tracing::info!(
                    "The public key {:?} in the database was matched with `{OLD_SEED}{old_seed}`.",
                    crate::encode_to_hex(old_public)
                );

                mapped_old_pairs.insert(old_public, old_pair);
                new_old_publics_death_timestamps.push((old_public, timestamp));
            }
            None if old_public == public => {
                if restored_key_timestamp.is_none() {
                    restored_key_timestamp = Some(timestamp.0);
                } else {
                    duplicate_message(old_public);
                }
            }
            None if mapped_old_pairs.contains_key(&old_public) => {
                duplicate_message(old_public);
            }
            None => {
                anyhow::bail!(
                    "public key {:?} that was removed on {} has no matching seed in `{OLD_SEED}*`",
                    crate::encode_to_hex(old_public),
                    format_timestamp(timestamp.0)
                );
            }
        }
    }

    if public != *db_public {
        if let Some(timestamp) = restored_key_timestamp {
            tracing::info!(
                "The current key {:?} will be changed to {:?} removed on {}.",
                crate::encode_to_hex(&db_public),
                crate::encode_to_hex(public),
                format_timestamp(timestamp)
            );
        } else {
            tracing::info!(
                "The current key {:?} will be changed to {:?}.",
                crate::encode_to_hex(&db_public),
                crate::encode_to_hex(public)
            );
        }

        let is_db_public_in_circulation = match open_ro_table(tx, KEYS)? {
            Some(keys) => keys
                .get(*db_public)
                .with_context(|| {
                    format!(
                        "failed to get a slot of the current key in the {:?} table",
                        KEYS.name()
                    )
                })?
                .is_some(),
            None => false,
        };

        if is_db_public_in_circulation {
            old_pairs
                .remove(db_public)
                .with_context(|| format!("current key has no matching seed in `{OLD_SEED}*`",))?;

            let timestamp: Timestamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .context("time travel is not supported, check the system time correctness")?
                .as_millis()
                .try_into()
                .context(
                    "system time is too far in the future, check the system time correctness",
                )?;

            new_old_publics_death_timestamps.push((public, timestamp.into()));
        } else {
            tracing::info!(
                "The current key has no accounts associated with it and hence will be deleted immediately."
            );
        }

        *db_public = public;
    }

    for (_, old_seed_name) in old_pairs.into_values() {
        tracing::warn!(
            "`{OLD_SEED}{old_seed_name:?}` has no matching public keys and thus is ignored."
        );
    }

    Ok((mapped_old_pairs, new_old_publics_death_timestamps))
}

struct ProcessChainParameters<'a> {
    currencies: &'a mut HashMap<Arc<String>, Option<AssetId>>,
    mapped_currencies: &'a mut HashMap<String, MappedCurrency>,
    mapped_chains: &'a mut HashMap<ChainHash, ChainInfo>,
    checked_chains: &'a mut Vec<(ChainHash, InnerConnectedChain)>,
    new_db_chains: &'a mut Vec<(String, ChainProperties)>,
}

fn process_db_chain(
    name: String,
    properties: &ChainProperties,
    chains: &mut HashMap<String, ConnectedChain>,
    ProcessChainParameters {
        currencies,
        mapped_currencies,
        mapped_chains,
        checked_chains,
        new_db_chains,
    }: ProcessChainParameters<'_>,
) -> Result<()> {
    let Entry::Vacant(entry) = mapped_chains.entry(properties.hash) else {
        tracing::debug!("Detected a chain hash duplicate {name:?} in the database.");

        return Ok(());
    };

    if let Some(connected_chain) = chains.remove(&name) {
        if connected_chain.genesis.0 != properties.genesis {
            anyhow::bail!(
                "chain {name:?} has different genesis hashes in the database & from an RPC server, check RPC server URLs for correctness"
            );
        }

        tracing::info!(
            "The {name:?} chain stored in the database is assigned to the {:?} hash.",
            crate::encode_to_hex(properties.hash)
        );

        process_maps_n_vecs(ProcessMapsNVecs {
            name,
            chain: connected_chain,
            chain_hash: properties.hash,
            currencies,
            mapped_currencies,
            checked_chains,
            new_db_chains,
            entry,
        });
    } else {
        tracing::warn!(
            "The {name:?} chain exists in the database but doesn't present in the config."
        );
    }

    Ok(())
}

struct ProcessMapsNVecs<'a> {
    name: String,
    chain: ConnectedChain,
    chain_hash: ChainHash,
    currencies: &'a mut HashMap<Arc<String>, Option<AssetId>>,
    mapped_currencies: &'a mut HashMap<String, MappedCurrency>,
    checked_chains: &'a mut Vec<(ChainHash, InnerConnectedChain)>,
    new_db_chains: &'a mut Vec<(String, ChainProperties)>,
    entry: VacantEntry<'a, ChainHash, ChainInfo>,
}

fn process_maps_n_vecs(
    ProcessMapsNVecs {
        name,
        chain,
        chain_hash,
        currencies,
        mapped_currencies,
        checked_chains,
        new_db_chains,
        entry,
    }: ProcessMapsNVecs<'_>,
) {
    for currency_name in chain.currencies {
        let asset = currencies
            .remove(&currency_name)
            .expect("`currencies` should've all `chain`'s currencies");

        mapped_currencies.insert(
            Arc::unwrap_or_clone(currency_name),
            MappedCurrency {
                chain: chain_hash,
                asset,
            },
        );
    }

    checked_chains.push((chain_hash, chain.inner));
    new_db_chains.push((
        name.clone(),
        ChainProperties {
            genesis: chain.genesis.0,
            hash: chain_hash,
        },
    ));
    entry.insert(ChainInfo {
        name,
        rpc: chain.rpc,
    });
}

fn process_chain(
    name: String,
    chain: ConnectedChain,
    ProcessChainParameters {
        currencies,
        mapped_currencies,
        mapped_chains,
        checked_chains,
        new_db_chains,
    }: ProcessChainParameters<'_>,
) {
    let mut chain_hash = U256::from(chain.genesis.0);
    let mut step = U256::one();

    loop {
        let hash = chain_hash.into();

        if let Entry::Vacant(entry) = mapped_chains.entry(hash) {
            tracing::info!(
                "A new {name:?} chain is assigned to the {:?} hash.",
                crate::encode_to_hex(hash)
            );

            process_maps_n_vecs(ProcessMapsNVecs {
                name,
                chain,
                chain_hash: hash,
                currencies,
                mapped_currencies,
                checked_chains,
                new_db_chains,
                entry,
            });

            break;
        }

        tracing::debug!(
            "Failed to assign a new {name:?} chain to the {:?} hash. Probing the next slot...",
            crate::encode_to_hex(hash)
        );

        chain_hash = chain_hash.overflowing_add(step).0;
        step = step
            .checked_add(U256::one())
            .expect("database can't store more than `U256::MAX` chains");
    }
}

fn open_ro_table<K: Key, V: Value>(
    tx: &ReadTransaction,
    table: TableDefinition<'_, K, V>,
) -> Result<Option<ReadOnlyTable<K, V>>> {
    tx.open_table(table)
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
                table.name()
            )
        })
        .map_err(Into::into)
}
