use crate::{
    arguments::Chain,
    database::Database,
    error::{Error, TaskError},
    utils::task_tracker::ShortTaskTracker,
};
use ahash::RandomState;
use indexmap::{map::Entry, IndexMap};
use std::sync::Arc;
use tokio::sync::{
    mpsc::{self, Sender as MpscSender},
    oneshot::{self, Sender as OsSender},
};

pub mod definitions;

mod rpc;

use definitions::{ChainConnector, ConnectedChain};

pub const MODULE: &str = module_path!();

pub async fn connect(
    chains: Vec<Chain>,
) -> Result<IndexMap<Arc<str>, ConnectedChain, RandomState>, Error> {
    let mut connected_chains = IndexMap::with_capacity_and_hasher(chains.len(), RandomState::new());
    let (connected_chains_tx, mut connected_chains_rx) = mpsc::channel::<(_, _, OsSender<_>)>(1);

    let connected_chains_jh = tokio::spawn(async move {
        while let Some((name, connected_chain, is_occupied)) = connected_chains_rx.recv().await {
            is_occupied
                .send(match connected_chains.entry(name) {
                    Entry::Occupied(_) => true,
                    Entry::Vacant(entry) => {
                        tracing::info!("Connected to the {:?} chain.", entry.key());

                        entry.insert(connected_chain);

                        false
                    }
                })
                .expect("receiver side should be alive");
        }

        connected_chains
    });

    let task_tracker = ShortTaskTracker::new();

    for chain in chains {
        task_tracker.spawn(
            ChainConnector(chain.name.clone()),
            connect_to_chain(chain, connected_chains_tx.clone()),
        );
    }

    // Drop the sender before `task_tracker.try_wait()` to prevent a deadlock.
    drop(connected_chains_tx);

    task_tracker.try_wait().await?;

    Ok(connected_chains_jh
        .await
        .expect("this `JoinHandle` shouldn't panic"))
}

#[tracing::instrument(skip_all, fields(chain = %chain.name))]
async fn connect_to_chain(
    chain: Chain,
    connected_chains: MpscSender<(Arc<str>, ConnectedChain, OsSender<bool>)>,
) -> Result<(), TaskError> {
    let first_endpoint = chain
        .config
        .endpoints
        .first()
        .ok_or(TaskError::NoChainEndpoints)?;
    let client = rpc::connect(first_endpoint).await?;
    let genesis = rpc::fetch_genesis_hash(&client).await?;

    tracing::debug!(genesis = %genesis.0.to_hex());

    let (is_occupied_tx, is_occupied_rx) = oneshot::channel();

    connected_chains
        .send((
            chain.name,
            ConnectedChain {
                config: chain.config,
                genesis,
                client,
            },
            is_occupied_tx,
        ))
        .await
        .expect("receiver side should be open");

    if is_occupied_rx
        .await
        .expect("sender side should send a value")
    {
        Err(TaskError::ChainDuplicate)
    } else {
        Ok(())
    }
}

#[allow(clippy::module_name_repetitions)]
pub struct ChainManager;

impl ChainManager {
    pub async fn new(
        database: Arc<Database>,
        chains: IndexMap<Arc<str>, ConnectedChain, RandomState>,
    ) -> Result<(), Error> {
        Ok(())
    }
}
