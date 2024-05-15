//! A tracker that follows individual chain

use std::collections::HashMap;

use frame_metadata::v15::RuntimeMetadataV15;
use jsonrpsee::ws_client::{WsClient, WsClientBuilder};
use jsonrpsee::core::client::{ClientT, Subscription, SubscriptionClientT};
use substrate_parser::ShortSpecs;
use tokio::{
    sync::{mpsc, oneshot},
    time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;

use crate::{
    TaskTracker,
    chain::{
        definitions::{BlockHash, ChainRequest, ChainTrackerRequest, EventFilter, Invoice},
        payout::payout, 
        rpc::{BlockHead, assets_set_at_block, block_hash, events_at_block, genesis_hash, metadata, next_block, next_block_number, specs, subscribe_blocks},
        utils::{events_entry_metadata, was_balance_received_at_account},
    },
    definitions::{Chain, api_v2::CurrencyProperties,},
    error::ErrorChain,
    signer::Signer,
    state::State,
};

pub fn start_chain_watch(
    c: Chain,
    currency_map: &HashMap<String, String>,
    chain_tx: mpsc::Sender<ChainTrackerRequest>,
    mut chain_rx: mpsc::Receiver<ChainTrackerRequest>,
    state: State,
    signer: Signer,
    task_tracker: TaskTracker,
    cancellation_token: CancellationToken,
) -> Result<(), ErrorChain> {
    //let (block_source_tx, mut block_source_rx) = mpsc::channel(16);
    task_tracker
        .clone()
        .spawn(format!("Chain {} watcher", c.name.clone()), async move {
            let watchdog = 30000;
            let mut watched_accounts = HashMap::new();
            let mut shutdown = false;
            for endpoint in c.endpoints.iter().cycle() {
                // not restarting chain if shutdown is in progress
                if shutdown || cancellation_token.is_cancelled() {
                    break;
                }
                if let Ok(client) = WsClientBuilder::default().build(endpoint).await {
                    // prepare chain
                    let watcher = match ChainWatcher::prepare_chain(&client, &mut watched_accounts, endpoint, chain_tx.clone(), state.interface(), task_tracker.clone())
                        .await
                    {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::info!(
                                "Failed to connect to chain {}, due to {} switching RPC server...",
                                c.name,
                                e
                            );
                            continue;
                        }
                    };


                    // fulfill requests
                    while let Ok(Some(request)) =
                        timeout(Duration::from_millis(watchdog), chain_rx.recv()).await
                    {
                        match request {
                            ChainTrackerRequest::NewBlock(block) => {
                                // TODO: hide this under rpc module
                                let block = match block_hash(&client, Some(block)).await {
                                    Ok(a) => a,
                                    Err(e) => {
                                        tracing::info!(
                                        "Failed to receive block in chain {}, due to {} switching RPC server...",
                                        c.name,
                                        e
                            );
                            continue;

                                    },
                                };
                                let events = events_at_block(
                                    &client,
                                    &block,
                                    Some(EventFilter {
                                        pallet: "Balances",
                                        optional_event_variant: Some("Transfer"),
                                    }),
                                    events_entry_metadata(&watcher.metadata)?,
                                    &watcher.metadata.types,
                                    )
                                    .await
                                    .unwrap();

                                let mut id_remove_list = Vec::new();
                                for (id, invoice) in watched_accounts.iter() {
                                    if events.iter().any(|event| was_balance_received_at_account(&invoice.address, &event.0.fields)) {
                                        match invoice.check(&client, &watcher, &block).await {
                                            Ok(true) => {
                                                state.order_paid(id.clone()).await;
                                                id_remove_list.push(id.to_owned());
                                            }
                                            Ok(false) => (),
                                            Err(e) => {
                                                tracing::warn!("account fetch error: {0:?}", e);
                                            }
                                        }
                                    }
                                }

                                for id in id_remove_list {
                                    watched_accounts.remove(&id);
                                }
                            }
                            ChainTrackerRequest::WatchAccount(request) => {
                                watched_accounts.insert(request.id.clone(), Invoice::from_request(request));
                            }
                            ChainTrackerRequest::Reap(request) => {
                                let id = request.id.clone();
                                let rpc = endpoint.clone();
                                let reap_state_handle = state.interface();
                                let watcher_for_reaper = watcher.clone();
                                let signer_for_reaper = signer.interface();
                                task_tracker.clone().spawn(format!("Initiate payout for order {}", id.clone()), async move {
                                    payout(rpc, Invoice::from_request(request), reap_state_handle, watcher_for_reaper, signer_for_reaper).await;
                                    Ok(format!("Payout attempt for order {} terminated", id).into())
                                });
                            }
                            ChainTrackerRequest::Shutdown(res) => {
                                shutdown = true;
                                let _ = res.send(());
                                break;
                            }
                        }
                    }
                }
            }
            Ok(format!("Chain {} monitor shut down", c.name).into())
        });
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ChainWatcher {
    pub genesis_hash: BlockHash,
    pub metadata: RuntimeMetadataV15,
    pub specs: ShortSpecs,
    pub assets: HashMap<String, CurrencyProperties>,
}

impl ChainWatcher {
    pub async fn prepare_chain(
        client: &WsClient,
        watched_accounts: &mut HashMap<String, Invoice>,
        rpc_url: &str,
        chain_tx: mpsc::Sender<ChainTrackerRequest>,
        state: State,
        task_tracker: TaskTracker,
    ) -> Result<Self, ErrorChain> {
        let genesis_hash = genesis_hash(&client).await?;
        let mut blocks = subscribe_blocks(&client).await?;
        let block = next_block(client, &mut blocks).await?;
        let metadata = metadata(&client, &block).await?;
        let specs = specs(&client, &metadata, &block).await?;
        let assets =
            assets_set_at_block(&client, &block, &metadata, rpc_url, specs.clone()).await?;

        let chain = ChainWatcher {
            genesis_hash,
            metadata,
            specs,
            assets,
        };

        // check monitored accounts
        let mut id_remove_list = Vec::new();
        for (id, account) in watched_accounts.iter() {
            match account.check(client, &chain, &block).await {
                Ok(true) => {
                    state.order_paid(id.clone()).await;
                    id_remove_list.push(id.to_owned());
                }
                Ok(false) => (),
                Err(e) => {
                    tracing::warn!("account fetch error: {0}", e);
                }
            }
        }
        for id in id_remove_list {
            watched_accounts.remove(&id);
        }

        task_tracker.spawn("watching blocks at {rpc_url}", async move {
            while let Ok(block) = next_block_number(&mut blocks).await {
                if chain_tx
                    .send(ChainTrackerRequest::NewBlock(block))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            // this should reset chain monitor on timeout;
            // but if this breaks, it meand that the latter is already down either way
            Ok("Block watch at {rpc_url} stopped".into())
        });

        Ok(chain)
    }
}


