use crate::{
    database::{Invoice, InvoiceStatus, ReadInvoices, State, WriteInvoices},
    unexpected_closure_of_notification_channel, Account, Asset, Balance, BatchedCallsLimit,
    BlockNumber, Decimals, Hash, Nonce, OnlineClient, RuntimeConfig, DECIMALS,
    SCANNER_TO_LISTENER_SWITCH_POINT,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};
use std::{
    collections::{hash_map::Entry, HashMap},
    error::Error,
    fmt::{self, Arguments, Display, Formatter, Write},
    sync::Arc,
};
use subxt::{
    backend::{
        legacy::{LegacyBackend, LegacyRpcMethods},
        rpc::{RpcClient, RpcSubscription},
        Backend, BackendExt, RuntimeVersion,
    },
    blocks::{Block, BlocksClient},
    config::{
        signed_extensions::{ChargeTransactionPaymentParams, CheckMortalityParams},
        Header,
    },
    constants::ConstantsClient,
    dynamic::{self, Value},
    error::RpcError,
    ext::{
        futures::TryFutureExt,
        scale_decode::DecodeAsType,
        scale_value::{self, At},
        sp_core::{
            crypto::{AccountId32, Ss58AddressFormat},
            sr25519::Pair,
        },
    },
    storage::{Storage, StorageClient},
    tx::{PairSigner, SubmittableExtrinsic, TxClient},
    Config, Metadata,
};
use tokio::sync::{watch::Receiver, RwLock};
use updater::Updater;

mod tx_manager;
mod updater;

pub const MODULE: &str = module_path!();

const MAX_BLOCK_NUMBER_ERROR: &str = "block number type overflow is occurred";
const BLOCK_NONCE_ERROR: &str = "failed to fetch an account nonce by the scanner client";

// Pallets

const SYSTEM: &str = "System";
const BALANCES: &str = "Balances";
const UTILITY: &str = "Utility";
const ASSETS: &str = "Assets";

async fn fetch_best_block(methods: &LegacyRpcMethods<RuntimeConfig>) -> Result<Hash> {
    methods
        .chain_get_block_hash(None)
        .await
        .context("failed to get the best block hash")?
        .context("received nothing after requesting the best block hash")
}

async fn fetch_runtime(
    methods: &LegacyRpcMethods<RuntimeConfig>,
    backend: &impl Backend<RuntimeConfig>,
) -> Result<(Metadata, RuntimeVersion)> {
    let best_block = fetch_best_block(methods).await?;

    Ok((
        fetch_metadata(backend, best_block)
            .await
            .context("failed to fetch metadata")?,
        methods
            .state_get_runtime_version(Some(best_block))
            .await
            .map(|runtime_version| RuntimeVersion {
                spec_version: runtime_version.spec_version,
                transaction_version: runtime_version.transaction_version,
            })
            .context("failed to fetch the runtime version")?,
    ))
}

async fn fetch_metadata(backend: &impl Backend<RuntimeConfig>, at: Hash) -> Result<Metadata> {
    const LATEST_SUPPORTED_METADATA_VERSION: u32 = 15;

    backend
        .metadata_at_version(LATEST_SUPPORTED_METADATA_VERSION, at)
        .or_else(|error| async {
            if let subxt::Error::Rpc(RpcError::ClientError(_)) | subxt::Error::Other(_) = error {
                backend.legacy_metadata(at).await
            } else {
                Err(error)
            }
        })
        .await
        .map_err(Into::into)
}

fn fetch_constant<T: DecodeAsType>(
    constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
    constant: (&str, &str),
) -> Result<T> {
    constants
        .at(&dynamic::constant(constant.0, constant.1))
        .with_context(|| format!("failed to get the constant {constant:?}"))?
        .as_type()
        .with_context(|| format!("failed to decode the constant {constant:?}"))
}

fn fetch_address_format(
    finalized_constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
) -> Result<Ss58AddressFormat> {
    const ADDRESS_PREFIX: (&str, &str) = (SYSTEM, "SS58Prefix");

    Ok(Ss58AddressFormat::custom(fetch_constant(
        finalized_constants,
        ADDRESS_PREFIX,
    )?))
}

async fn fetch_finalized_head_number_and_hash(
    methods: &LegacyRpcMethods<RuntimeConfig>,
) -> Result<(BlockNumber, Hash)> {
    let head_hash = methods
        .chain_get_finalized_head()
        .await
        .context("failed to get the finalized head hash")?;
    let head = methods
        .chain_get_block(Some(head_hash))
        .await
        .context("failed to get the finalized head")?
        .context("received nothing after requesting the finalized head")?;

    Ok((head.block.header.number, head_hash))
}

#[derive(Debug)]
pub struct ApiProperties {
    pub block_hash_count: BlockNumber,
    pub batched_calls_limit: BatchedCallsLimit,
}

impl ApiProperties {
    fn fetch(
        constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
        no_utility: bool,
    ) -> Result<Self> {
        const BLOCK_HASH_COUNT: (&str, &str) = (SYSTEM, "BlockHashCount");
        const BATCHED_CALLS_LIMIT: (&str, &str) = (UTILITY, "BatchedCallsLimit");
        const NO_UTILITY: BatchedCallsLimit = 1;

        Ok(Self {
            block_hash_count: fetch_constant(constants, BLOCK_HASH_COUNT)?,
            batched_calls_limit: if no_utility {
                NO_UTILITY
            } else {
                fetch_constant(constants, BATCHED_CALLS_LIMIT)?
            },
        })
    }
}

pub struct ChainProperties {
    pub address_format: Ss58AddressFormat,
    pub existential_deposit: Balance,
}

// impl ChainProperties {
//     fn native(
//         address_format: Ss58AddressFormat,
//         best: ChainPropertiesBest,
//         finalized_constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
//     ) -> Result<Self> {
//         const EXISTENTIAL_DEPOSIT: (&str, &str) = (BALANCES, "ExistentialDeposit");

//         Ok(Self {
//             best,
//             finalized: ChainPropertiesFinalized {
//                 address_format,
//                 existential_deposit: fetch_constant(finalized_constants, EXISTENTIAL_DEPOSIT)?,
//             },
//         })
//     }

//     async fn asset(
//         address_format: Ss58AddressFormat,
//         best: ChainPropertiesBest,
//         finalized_storage: &Storage<RuntimeConfig, OnlineClient>,
//         asset: Asset,
//     ) -> Result<Self> {
//         const ASSET: &str = "Asset";
//         const MIN_BALANCE: &str = "min_balance";

//         let asset_properties = finalized_storage
//             .fetch(&dynamic::storage(ASSETS, ASSET, vec![asset]))
//             .await
//             .context("failed to get asset properties")?
//             .context("asset with the given identifier doesn't exist")?
//             .to_value()
//             .context("failed to decode asset properties")?;
//         let encoded_min_balance = asset_properties
//             .at(MIN_BALANCE)
//             .with_context(|| format!("{MIN_BALANCE} field wasn't found in asset properties"))?;

//         Ok(Self {
//             best,
//             finalized: ChainPropertiesFinalized {
//                 address_format,
//                 existential_deposit: encoded_min_balance
//                     .as_u128()
//                     .with_context(|| {
//                         format!(
//                             "expected `u128` as the type of the {MIN_BALANCE} field, got {encoded_min_balance}"
//                         )
//                     })?,
//             },
//         })
//     }
// }

pub struct ApiConfig {
    api: Arc<OnlineClient>,
    methods: Arc<LegacyRpcMethods<RuntimeConfig>>,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
}

pub struct ScannerConfig {
    api: Arc<OnlineClient>,
    methods: Arc<LegacyRpcMethods<RuntimeConfig>>,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
    api_properties: Arc<RwLock<ApiProperties>>,
}

pub struct EndpointProperties {
    pub url: CheckedUrl,
    pub chain: Arc<RwLock<ChainProperties>>,
}

pub struct CheckedUrl(String);

impl CheckedUrl {
    pub fn get(self) -> String {
        self.0
    }
}

pub async fn prepare(
    url: String,
    asset: Option<Asset>,
    no_utility: bool,
    shutdown_notification: Receiver<bool>,
) -> Result<(ScannerConfig, EndpointProperties, Updater, BlockNumber)> {
    let rpc = RpcClient::from_url(&url)
        .await
        .context("failed to construct the RPC client")?;

    log::info!("Connected to an RPC server at \"{url}\".");

    let methods = Arc::new(LegacyRpcMethods::new(rpc.clone()));
    let backend = Arc::new(LegacyBackend::new(rpc));

    let (metadata, runtime_version) = fetch_runtime(&methods, &*backend)
        .await
        .context("failed to fetch the runtime of the API client")?;
    let genesis_hash = methods
        .genesis_hash()
        .await
        .context("failed to get the genesis hash")?;
    let client = Arc::new(
        OnlineClient::from_backend_with(genesis_hash, runtime_version, metadata, backend.clone())
            .context("failed to construct the API client")?,
    );
    let constants = client.constants();

    let api_properties = ApiProperties::fetch(&constants, no_utility)?;

    log::debug!("API client properties: {api_properties:?}.");

    let address_format = fetch_address_format(finalized_constants)

    log::info!(
        "Chain properties:\n\
         Decimal places number: {}.\n\
         Address format: \"{}\" ({}).\n\
         Existential deposit: {}.\n\
         Block hash count: {}.",
        properties.decimals,
        properties.address_format,
        properties.address_format.prefix(),
        properties.existential_deposit,
        properties.block_hash_count
    );

    let arc_properties = Arc::new(RwLock::const_new(properties));

    Ok((
        ScannerConfig {
            api: client.clone(),
            methods: methods.clone(),
            backend: backend.clone(),
        },
        EndpointProperties {
            url: CheckedUrl(url),
            chain: todo!(),
        },
        Updater {
            client,
            methods,
            backend,
            shutdown_notification,
            properties: todo!(),
            constants,
            no_utility,
        },
        fetch_finalized_head_number_and_hash(&methods).await?.0
    ))
}

#[derive(Debug)]
struct Shutdown;

impl Error for Shutdown {}

// Not used, but required for the `anyhow::Context` trait.
impl Display for Shutdown {
    fn fmt(&self, _: &mut Formatter<'_>) -> fmt::Result {
        unimplemented!()
    }
}

struct Api {
    tx: TxClient<RuntimeConfig, OnlineClient>,
}

struct Scanner {
    client: OnlineClient,
    blocks: BlocksClient<RuntimeConfig, OnlineClient>,
    storage: StorageClient<RuntimeConfig, OnlineClient>,
}

pub struct Processor {
    api: Api,
    scanner: Scanner,
    methods: Arc<LegacyRpcMethods<RuntimeConfig>>,
    database: Arc<State>,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
    shutdown_notification: Receiver<bool>,
}

impl Processor {
    pub fn new(
        ApiConfig {
            api,
            methods,
            backend,
        }: ApiConfig,
        database: Arc<State>,
        shutdown_notification: Receiver<bool>,
    ) -> Result<Self> {
        let scanner = OnlineClient::from_backend_with(
            api.genesis_hash(),
            api.runtime_version(),
            api.metadata(),
            backend.clone(),
        )
        .context("failed to initialize the scanner client")?;

        Ok(Processor {
            api: Api { tx: api.tx() },
            scanner: Scanner {
                blocks: scanner.blocks(),
                storage: scanner.storage(),
                client: scanner,
            },
            methods,
            database,
            shutdown_notification,
            backend,
        })
    }

    pub async fn ignite(self, latest_saved_block: Option<BlockNumber>) -> Result<&'static str> {
        self.execute(latest_saved_block).await.or_else(|error| {
            error
                .downcast()
                .map(|Shutdown| "The RPC module is shut down.")
        })
    }

    async fn execute(mut self, latest_saved_block: Option<BlockNumber>) -> Result<&'static str> {
        let (mut head_number, head_hash) = self
            .finalized_head_number_and_hash()
            .await
            .context("failed to get the chain head")?;

        let mut next_unscanned_number;
        let mut subscription;

        if let Some(latest_saved) = latest_saved_block {
            let latest_saved_hash = self
                .methods
                .chain_get_block_hash(Some(latest_saved.into()))
                .await
                .context("failed to get the hash of the last saved block")?
                .context("received nothing after requesting the hash of the last saved block")?;

            self.set_scanner_metadata(latest_saved_hash).await?;

            next_unscanned_number = latest_saved
                .checked_add(1)
                .context(MAX_BLOCK_NUMBER_ERROR)?;

            let mut unscanned_amount = head_number.saturating_sub(next_unscanned_number);

            if unscanned_amount >= SCANNER_TO_LISTENER_SWITCH_POINT {
                log::info!(
                    "Detected {unscanned_amount} unscanned blocks! Catching up may take a while."
                );

                while unscanned_amount >= SCANNER_TO_LISTENER_SWITCH_POINT {
                    self.process_skipped(&mut next_unscanned_number, head_number)
                        .await
                        .context("failed to process a skipped gap in the scanning mode")?;

                    (head_number, _) = self
                        .finalized_head_number_and_hash()
                        .await
                        .context("failed to get a new chain head")?;
                    unscanned_amount = head_number.saturating_sub(next_unscanned_number);
                }

                log::info!(
                    "Scanning of skipped blocks has been completed! Switching to the listening mode..."
                );
            }

            subscription = self.finalized_heads().await?;
        } else {
            self.set_scanner_metadata(head_hash).await?;

            next_unscanned_number = head_number.checked_add(1).context(MAX_BLOCK_NUMBER_ERROR)?;
            subscription = self.finalized_heads().await?;
        }

        // Skip all already scanned blocks in cases like the first startup (we always skip the first
        // block to fetch right metadata), an instant daemon restart, or a connection to a lagging
        // endpoint.
        'skipping: loop {
            loop {
                tokio::select! {
                    biased;
                    notification = self.shutdown_notification.changed() => {
                        return notification
                            .with_context(|| unexpected_closure_of_notification_channel("skipping"))
                            .and_then(|()| Err(Shutdown.into()));
                    }
                    header_result_option = subscription.next() => {
                        if let Some(header_result) = header_result_option {
                            let header = header_result.context(
                                "received an error from the RPC client while skipping saved finalized heads"
                            )?;

                            if header.number >= next_unscanned_number {
                                break 'skipping;
                            }
                        } else {
                            break;
                        }
                    }
                }
            }

            log::warn!("Lost the connection while skipping already scanned blocks. Retrying...");

            subscription = self
                .finalized_heads()
                .await
                .context("failed to update the subscription while skipping scanned blocks")?;
        }

        loop {
            self.process_finalized_heads(subscription, &mut next_unscanned_number)
                .await?;

            log::warn!("Lost the connection while processing finalized heads. Retrying...");

            subscription = self
                .finalized_heads()
                .await
                .context("failed to update the subscription while processing finalized heads")?;
        }
    }

    async fn finalized_head_number_and_hash(&self) -> Result<(BlockNumber, Hash)> {
        let head_hash = self
            .methods
            .chain_get_finalized_head()
            .await
            .context("failed to get the finalized head hash")?;
        let head = self
            .methods
            .chain_get_block(Some(head_hash))
            .await
            .context("failed to get the finalized head")?
            .context("received nothing after requesting the finalized head")?;

        Ok((head.block.header.number, head_hash))
    }

    async fn set_scanner_metadata(&self, at: Hash) -> Result<()> {
        let metadata = fetch_metadata(&*self.backend, at)
            .await
            .context("failed to fetch metadata for the scanner client")?;

        self.scanner.client.set_metadata(metadata);

        Ok(())
    }

    async fn finalized_heads(&self) -> Result<RpcSubscription<<RuntimeConfig as Config>::Header>> {
        self.methods
            .chain_subscribe_finalized_heads()
            .await
            .context("failed to subscribe to finalized heads")
    }

    async fn process_skipped(
        &self,
        next_unscanned: &mut BlockNumber,
        head: BlockNumber,
    ) -> Result<()> {
        for skipped_number in *next_unscanned..head {
            if self.shutdown_notification.has_changed().with_context(|| {
                unexpected_closure_of_notification_channel("skipped blocks processing")
            })? {
                return Err(Shutdown.into());
            }

            let skipped_hash = self
                .methods
                .chain_get_block_hash(Some(skipped_number.into()))
                .await
                .context("failed to get the hash of a skipped block")?
                .context("received nothing after requesting the hash of a skipped block")?;

            self.process_block(skipped_number, skipped_hash).await?;
        }

        *next_unscanned = head;

        Ok(())
    }

    async fn process_finalized_heads(
        &mut self,
        mut subscription: RpcSubscription<<RuntimeConfig as Config>::Header>,
        next_unscanned: &mut BlockNumber,
    ) -> Result<()> {
        loop {
            tokio::select! {
                biased;
                notification = self.shutdown_notification.changed() => {
                    return notification
                        .with_context(|| {
                            unexpected_closure_of_notification_channel("finalized heads processing")
                        })
                        .and_then(|()| Err(Shutdown.into()));
                }
                head_result_option = subscription.next() => {
                    if let Some(head_result) = head_result_option {
                        let head = head_result.context(
                            "received an error from the RPC client while processing finalized heads"
                        )?;

                        self
                            .process_skipped(next_unscanned, head.number)
                            .await
                            .context("failed to process a skipped gap in the listening mode")?;
                        self.process_block(head.number, head.hash()).await?;

                        *next_unscanned = head.number
                            .checked_add(1)
                            .context(MAX_BLOCK_NUMBER_ERROR)?;
                    } else {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn process_block(&self, number: BlockNumber, hash: Hash) -> Result<()> {
        log::info!("Processing the block: {number}.");

        let block = self
            .scanner
            .blocks
            .at(hash)
            .await
            .context("failed to obtain a block for processing")?;
        let events = block
            .events()
            .await
            .context("failed to obtain block events")?;

        let read_tx = self.database.read()?;
        let read_invoices = read_tx.invoices()?;

        let mut update = false;
        let mut invoices_changes = HashMap::new();

        for event_result in events.iter() {
            let event = event_result.context("failed to decode an event")?;
            let metadata = event.event_metadata();

            const UPDATE: &str = "CodeUpdated";
            const TRANSFER: &str = "Transfer";

            match (metadata.pallet.name(), &*metadata.variant.name) {
                (SYSTEM, UPDATE) => update = true,
                (BALANCES, TRANSFER) => Transfer::deserialize(
                    event
                        .field_values()
                        .context("failed to decode event's fields")?,
                )
                .context("failed to deserialize a transfer event")?
                .process(&mut invoices_changes, &read_invoices)?,
                _ => {}
            }
        }

        let write_tx = self.database.write()?;
        let mut write_invoices = write_tx.invoices()?;

        for (invoice, changes) in invoices_changes {
            // match changes.invoice.status {
            //     InvoiceStatus::Unpaid(price) => self
            //         .process_unpaid(&block, changes, hash, invoice, price, &mut write_invoices)
            //         .await
            //         .context("failed to process an unpaid invoice")?,
            //     InvoiceStatus::Paid(_) => self
            //         .process_paid(invoice, &block, changes, hash)
            //         .await
            //         .context("failed to process a paid invoice")?,
            // }
        }

        drop(write_invoices);

        write_tx.root()?.save_last_block(number)?;
        write_tx.commit()?;

        if update {
            self.set_scanner_metadata(hash)
                .await
                .context("failed to update metadata in the scanner client")?;

            log::info!("A metadata update has been found and applied for the scanner client.");
        }

        Ok(())
    }

    async fn balance(&self, hash: Hash, account: &Account) -> Result<Balance> {
        const ACCOUNT: &str = "Account";
        const ACCOUNT_BALANCES: &str = "data";
        const FREE_BALANCE: &str = "free";

        let account_info = self
            .scanner
            .storage
            .at(hash)
            .fetch_or_default(&dynamic::storage(
                SYSTEM,
                ACCOUNT,
                vec![AsRef::<[u8; 32]>::as_ref(account)],
            ))
            .await
            .context("failed to fetch account info from the chain")?
            .to_value()
            .context("failed to decode account info")?;
        let encoded_balance = account_info
            .at(ACCOUNT_BALANCES)
            .with_context(|| format!("{ACCOUNT_BALANCES} field wasn't found in account info"))?
            .at(FREE_BALANCE)
            .with_context(|| format!("{FREE_BALANCE} wasn't found in account balance info"))?;

        encoded_balance.as_u128().with_context(|| {
            format!("expected `u128` as the type of a free balance, got {encoded_balance}")
        })
    }

    async fn batch_transfer(
        &self,
        nonce: Nonce,
        block_hash_count: BlockNumber,
        signer: &PairSigner<RuntimeConfig, Pair>,
        transfers: Vec<Value>,
    ) -> Result<SubmittableExtrinsic<RuntimeConfig, OnlineClient>> {
        const FORCE_BATCH: &str = "force_batch";

        let call = dynamic::tx(UTILITY, FORCE_BATCH, vec![Value::from(transfers)]);
        let (number, hash) = self
            .finalized_head_number_and_hash()
            .await
            .context("failed to get the chain head while constructing a transaction")?;
        // let extensions = (
        //     (),
        //     (),
        //     (),
        //     (),
        //     CheckMortalityParams::mortal(block_hash_count.into(), number.into(), hash),
        //     ChargeTransactionPaymentParams::no_tip(),
        // );

        // self.api
        //     .tx
        //     .create_signed_with_nonce(&call, signer, nonce, extensions)
        //     .context("failed to create a transfer transaction")

        todo!()
    }

    async fn current_nonce(&self, account: &Account) -> Result<Nonce> {
        // self.api
        //     .blocks
        //     .at(fetch_best_block(&self.methods).await?)
        //     .await
        //     .context("failed to obtain the best block for fetching an account nonce")?
        //     .account_nonce(account)
        //     .await
        //     .context("failed to fetch an account nonce by the API client")

        todo!()
    }

    async fn process_unpaid(
        &self,
        block: &Block<RuntimeConfig, OnlineClient>,
        mut changes: InvoiceChanges,
        hash: Hash,
        invoice: Account,
        price: Balance,
        invoices: &mut WriteInvoices<'_, '_>,
    ) -> Result<()> {
        let balance = self.balance(hash, &invoice).await?;

        if let Some(remaining) = balance.checked_sub(price) {
            // changes.invoice.status = InvoiceStatus::Paid(price);

            let block_nonce = block
                .account_nonce(&invoice)
                .await
                .context(BLOCK_NONCE_ERROR)?;
            let current_nonce = self.current_nonce(&invoice).await?;

            // if current_nonce <= block_nonce {
            //     let properties = self.database.properties().await;
            //     let block_hash_count = properties.block_hash_count;
            //     let signer = changes.invoice.signer(self.database.pair())?;

            //     let mut transfers = vec![construct_transfer(&changes.invoice.recipient, price)];
            //     let mut tx = self
            //         .batch_transfer(current_nonce, block_hash_count, &signer, transfers.clone())
            //         .await?;
            //     let mut fee = calculate_estimate_fee(&tx).await?;

            //     if let Some(a) = (fee + properties.existential_deposit + price).checked_sub(balance)
            //     {
            //         let price_mod = price - a;

            //         transfers = vec![construct_transfer(&changes.invoice.recipient, price_mod)];
            //         tx = self
            //             .batch_transfer(current_nonce, block_hash_count, &signer, transfers.clone())
            //             .await?;

            //         self.methods
            //             .author_submit_extrinsic(tx.encoded())
            //             .await
            //             .context("failed to submit an extrinsic")?;
            //     } else if let Some((account, amount)) = changes.incoming.into_iter().next() {
            //         let mut temp_transfers = transfers.clone();

            //         temp_transfers.push(construct_transfer(&account, amount));
            //         tx = self
            //             .batch_transfer(
            //                 current_nonce,
            //                 block_hash_count,
            //                 &signer,
            //                 temp_transfers.clone(),
            //             )
            //             .await?;
            //         fee = calculate_estimate_fee(&tx).await?;

            //         if let Some(a) =
            //             (fee + properties.existential_deposit + amount).checked_sub(remaining)
            //         {
            //             let amount_mod = amount - a;

            //             transfers.push(construct_transfer(&account, amount_mod));
            //             tx = self
            //                 .batch_transfer(
            //                     current_nonce,
            //                     block_hash_count,
            //                     &signer,
            //                     transfers.clone(),
            //                 )
            //                 .await?;

            //             self.methods
            //                 .author_submit_extrinsic(tx.encoded())
            //                 .await
            //                 .context("failed to submit an extrinsic")?;
            //         } else {
            //             self.methods
            //                 .author_submit_extrinsic(tx.encoded())
            //                 .await
            //                 .context("failed to submit an extrinsic")?;
            //         }
            //     } else {
            //         self.methods
            //             .author_submit_extrinsic(tx.encoded())
            //             .await
            //             .context("failed to submit an extrinsic")?;
            //     }
            // }

            // invoices.save(&invoice, &changes.invoice)?;
        }

        Ok(())
    }

    async fn process_paid(
        &self,
        _invoice: Account,
        _block: &Block<RuntimeConfig, OnlineClient>,
        _changes: InvoiceChanges,
        _hash: Hash,
    ) -> Result<()> {
        Ok(())
    }
}

fn construct_transfer(to: &Account, amount: Balance) -> Value {
    const TRANSFER_KEEP_ALIVE: &str = "transfer_keep_alive";

    dynamic::tx(
        BALANCES,
        TRANSFER_KEEP_ALIVE,
        vec![
            scale_value::value!(Id(Value::from_bytes(to))),
            amount.into(),
        ],
    )
    .into_value()
}

async fn calculate_estimate_fee(
    extrinsic: &SubmittableExtrinsic<RuntimeConfig, OnlineClient>,
) -> Result<Balance> {
    extrinsic
        .partial_fee_estimate()
        .await
        .map(|f| f * 2)
        .context("failed to obtain a transfer's estimate fee")
}

struct InvoiceChanges {
    invoice: Invoice,
    incoming: HashMap<Account, Balance>,
}

#[derive(Deserialize)]
struct Transfer {
    // The implementation of `Deserialize` for `AccountId32` works only with strings.
    #[serde(deserialize_with = "account_deserializer")]
    from: AccountId32,
    #[serde(deserialize_with = "account_deserializer")]
    to: AccountId32,
    amount: Balance,
}

fn account_deserializer<'de, D>(deserializer: D) -> Result<AccountId32, D::Error>
where
    D: Deserializer<'de>,
{
    <([u8; 32],)>::deserialize(deserializer).map(|address| AccountId32::new(address.0))
}

impl Transfer {
    fn process(
        self,
        invoices_changes: &mut HashMap<Account, InvoiceChanges>,
        invoices: &ReadInvoices<'_>,
    ) -> Result<()> {
        if self.from == self.to || self.amount == 0 {
            return Ok(());
        }

        match invoices_changes.entry(self.to) {
            Entry::Occupied(entry) => {
                entry
                    .into_mut()
                    .incoming
                    .entry(self.from)
                    .and_modify(|amount| *amount = amount.saturating_add(self.amount))
                    .or_insert(self.amount);
            }
            Entry::Vacant(entry) => {
                if let (None, Some(encoded_invoice)) =
                    (invoices.get(&self.from)?, invoices.get(entry.key())?)
                {
                    entry.insert(InvoiceChanges {
                        invoice: encoded_invoice.value(),
                        incoming: [(self.from, self.amount)].into(),
                    });
                }
            }
        }

        Ok(())
    }
}
