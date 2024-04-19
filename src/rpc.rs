use crate::{
    asset::Asset,
    database::{Invoicee, State},
    server::{
        CurrencyInfo, CurrencyProperties, OrderInfo, OrderStatus, PaymentStatus, ServerInfo,
        TokenKind, WithdrawalStatus,
    },
    AccountId, AssetId, AssetInfo, Balance, BlockHash, BlockNumber, Chain, Decimals, NativeToken,
    Nonce, OnlineClient, PalletIndex, RuntimeConfig, TaskTracker, Timestamp,
};
use anyhow::{Context, Result};
use scale_info::TypeDef;
use serde::{Deserialize, Deserializer};
use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap},
    error::Error,
    fmt::{self, Debug, Display, Formatter},
    num::NonZeroU64,
    sync::Arc,
};
use subxt::{
    backend::{
        legacy::{LegacyBackend, LegacyRpcMethods},
        rpc::{reconnecting_rpc_client::Client, RpcClient, RpcSubscription},
        Backend, BackendExt, RuntimeVersion,
    },
    blocks::Block,
    config::{DefaultExtrinsicParamsBuilder, Header},
    constants::ConstantsClient,
    dynamic::{self, At, Value},
    error::RpcError,
    ext::{
        futures::TryFutureExt,
        scale_decode::DecodeAsType,
        scale_value,
        sp_core::{
            crypto::{Ss58AddressFormat, Ss58Codec},
            sr25519::Pair,
        },
    },
    runtime_api::RuntimeApiClient,
    storage::{Storage, StorageClient},
    tx::{PairSigner, SubmittableExtrinsic},
    Config, Metadata,
};
use tokio::sync::{
    mpsc::{self, UnboundedSender},
    oneshot::{self, Sender},
};
use tokio_util::sync::CancellationToken;

pub const MODULE: &str = module_path!();

const MAX_BLOCK_NUMBER_ERROR: &str = "block number type overflow is occurred";
const BLOCK_NONCE_ERROR: &str = "failed to fetch an account nonce by the scanner client";

const CHARGE_ASSET_TX_PAYMENT: &str = "ChargeAssetTxPayment";

// Pallets

const SYSTEM: &str = "System";
const BALANCES: &str = "Balances";
const UTILITY: &str = "Utility";
const ASSETS: &str = "Assets";
const BABE: &str = "Babe";

// Runtime APIs

const AURA: &str = "AuraApi";

type ConnectedChainsChannel = (
    Sender<Option<(String, ConnectedChain)>>,
    (String, ConnectedChain),
);

type CurrenciesChannel = (
    Sender<Option<(String, CurrencyProperties)>>,
    (String, CurrencyProperties),
);

struct AssetsInfoFetcher<'a> {
    assets: (AssetInfo, Vec<AssetInfo>),
    storage: &'a Storage<RuntimeConfig, OnlineClient>,
    pallet_index: Option<PalletIndex>,
}

async fn fetch_finalized_head_number_and_hash(
    methods: &LegacyRpcMethods<RuntimeConfig>,
) -> Result<(BlockNumber, BlockHash)> {
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

async fn fetch_runtime(
    methods: &LegacyRpcMethods<RuntimeConfig>,
    backend: &impl Backend<RuntimeConfig>,
    at: BlockHash,
) -> Result<(Metadata, RuntimeVersion)> {
    Ok((
        fetch_metadata(backend, at)
            .await
            .context("failed to fetch metadata")?,
        methods
            .state_get_runtime_version(Some(at))
            .await
            .map(|runtime_version| RuntimeVersion {
                spec_version: runtime_version.spec_version,
                transaction_version: runtime_version.transaction_version,
            })
            .context("failed to fetch the runtime version")?,
    ))
}

async fn fetch_metadata(backend: &impl Backend<RuntimeConfig>, at: BlockHash) -> Result<Metadata> {
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

#[derive(Debug)]
struct ChainProperties {
    address_format: Ss58AddressFormat,
    existential_deposit: Option<Balance>,
    assets_pallet: Option<AssetsPallet>,
    block_hash_count: BlockNumber,
    account_lifetime: BlockNumber,
    depth: Option<NonZeroU64>,
}

#[derive(Debug)]
struct AssetsPallet {
    multi_location: Option<PalletIndex>,
    assets: HashMap<AssetId, AssetProperties>,
}

#[derive(Debug)]
struct AssetProperties {
    min_balance: Balance,
    decimals: Decimals,
}

impl AssetProperties {
    async fn fetch(storage: &Storage<RuntimeConfig, OnlineClient>, asset: AssetId) -> Result<Self> {
        Ok(Self {
            min_balance: check_sufficiency_and_fetch_min_balance(storage, asset).await?,
            decimals: fetch_asset_decimals(storage, asset).await?,
        })
    }
}

impl ChainProperties {
    async fn fetch(
        chain: &str,
        currencies: UnboundedSender<CurrenciesChannel>,
        constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
        native_token_option: Option<NativeToken>,
        assets_fetcher: Option<AssetsInfoFetcher<'_>>,
        account_lifetime: BlockNumber,
        depth: Option<NonZeroU64>,
    ) -> Result<Self> {
        const ADDRESS_PREFIX: (&str, &str) = (SYSTEM, "SS58Prefix");
        const EXISTENTIAL_DEPOSIT: (&str, &str) = (BALANCES, "ExistentialDeposit");
        const BLOCK_HASH_COUNT: (&str, &str) = (SYSTEM, "BlockHashCount");

        /* wtf is this?
        let try_add_currency = |name, asset| async move {
            let (tx, rx) = oneshot::channel();

            currencies
                .send((tx, (name, CurrencyProperties {
                    chain_name: chain.to_owned(),
                    kind: ,
                    decimals: ,
                    rpc_url: ,
                    asset_if: ,
                })))
                .unwrap();

            if let Some((
                name,
                Currency {
                    chain: other_chain, ..
                },
            )) = rx.await.unwrap()
            {
                Err(anyhow::anyhow!(
                        "chain {other_chain:?} already has the native token or an asset with the name {name:?}, all currency names must be unique"
                    ))
            } else {
                Ok(())
            }
        };*/

        let assets_pallet = if let Some(AssetsInfoFetcher {
            assets: (last_asset_info, assets_info),
            storage,
            pallet_index,
        }) = assets_fetcher
        {
            async fn try_add_asset(
                assets: &mut HashMap<AssetId, AssetProperties>,
                id: AssetId,
                chain: &str,
                storage: &Storage<RuntimeConfig, OnlineClient>,
            ) -> Result<()> {
                match assets.entry(id) {
                    Entry::Occupied(_) => Err(anyhow::anyhow!(
                        "chain {chain} has 2 assets with the same ID {id}",
                    )),
                    Entry::Vacant(entry) => {
                        entry.insert(AssetProperties::fetch(storage, id).await?);

                        Ok(())
                    }
                }
            }

            let mut assets = HashMap::with_capacity(assets_info.len().saturating_add(1));

            for asset_info in assets_info {
                //try_add_currency.clone()(asset_info.name, Some(asset_info.id)).await?;
                try_add_asset(&mut assets, asset_info.id, chain, storage).await?;
            }

            //try_add_currency.clone()(last_asset_info.name, Some(last_asset_info.id)).await?;
            try_add_asset(&mut assets, last_asset_info.id, chain, storage).await?;

            Some(AssetsPallet {
                assets,
                multi_location: pallet_index,
            })
        } else {
            None
        };

        let address_format = Ss58AddressFormat::custom(fetch_constant(constants, ADDRESS_PREFIX)?);
        let block_hash_count = fetch_constant(constants, BLOCK_HASH_COUNT)?;

        //Some(native_token.decimals),

        let existential_deposit = if let Some(native_token) = native_token_option {
            Some(fetch_constant(constants, EXISTENTIAL_DEPOSIT).map(Balance)?)
        } else {
            None
        };

        let chain = Self {
            address_format,
            existential_deposit,
            assets_pallet,
            block_hash_count,
            account_lifetime,
            depth,
        };

        Ok(chain)
    }
}

async fn check_sufficiency_and_fetch_min_balance(
    storage: &Storage<RuntimeConfig, OnlineClient>,
    asset: AssetId,
) -> Result<Balance> {
    const ASSET: &str = "Asset";
    const MIN_BALANCE: &str = "min_balance";
    const IS_SUFFICIENT: &str = "is_sufficient";

    let asset_info = storage
        .fetch(&dynamic::storage(ASSETS, ASSET, vec![asset.into()]))
        .await
        .with_context(|| format!("failed to fetch asset {asset} info from a chain"))?
        .with_context(|| {
            format!("received nothing after fetching asset info {asset} from a chain")
        })?
        .to_value()
        .with_context(|| format!("failed to decode asset {asset} info"))?;

    let encoded_is_sufficient = asset_info
        .at(IS_SUFFICIENT)
        .with_context(|| format!("{IS_SUFFICIENT} field wasn't found in asset {asset} info"))?;

    if !encoded_is_sufficient.as_bool().with_context(|| {
        format!(
            "expected `bool` as the type of {IS_SUFFICIENT:?} in asset {asset} info, got `{:?}`",
            encoded_is_sufficient.value
        )
    })? {
        anyhow::bail!("only sufficient assets are supported, asset {asset} isn't sufficient");
    }

    let encoded_min_balance = asset_info
        .at(MIN_BALANCE)
        .with_context(|| format!("{MIN_BALANCE} field wasn't found in asset {asset} info"))?;

    encoded_min_balance.as_u128().map(Balance).with_context(|| {
        format!(
            "expected `u128` as the type of {MIN_BALANCE:?} in asset {asset} info, got `{:?}`",
            encoded_min_balance.value
        )
    })
}

async fn fetch_asset_decimals(
    storage: &Storage<RuntimeConfig, OnlineClient>,
    asset: AssetId,
) -> Result<Decimals> {
    const METADATA: &str = "Metadata";
    const DECIMALS: &str = "decimals";

    let asset_metadata = storage
        .fetch(&dynamic::storage(ASSETS, METADATA, vec![asset.into()]))
        .await
        .with_context(|| format!("failed to fetch asset {asset} metadata from a chain"))?
        .with_context(|| {
            format!("received nothing after fetching asset {asset} metadata from a chain")
        })?
        .to_value()
        .with_context(|| format!("failed to decode asset {asset} metadata"))?;
    let encoded_decimals = asset_metadata
        .at(DECIMALS)
        .with_context(|| format!("{DECIMALS} field wasn't found in asset {asset} metadata"))?;

    let decimals = encoded_decimals.as_u128().with_context(|| {
        format!(
            "expected `u128` as the type of asset {asset} {DECIMALS:?}, got `{:?}`",
            encoded_decimals.value
        )
    })?;

    decimals.try_into().with_context(|| {
        format!("asset {asset} {DECIMALS:?} must be less than `u8`, got {decimals}")
    })
}

pub async fn prepare(
    chains: Vec<Chain>,
    account_lifetime: Timestamp,
    depth: Option<Timestamp>,
) -> Result<(
    HashMap<String, ConnectedChain>,
    HashMap<String, CurrencyProperties>,
)> {
    let mut connected_chains = HashMap::with_capacity(chains.len());
    let mut currencies = HashMap::with_capacity(
        chains
            .iter()
            .map(|chain| {
                chain
                    .asset
                    .as_ref()
                    .map(Vec::len)
                    .unwrap_or_default()
                    .saturating_add(chain.native_token.is_some().into())
            })
            .sum(),
    );

    let (connected_chains_tx, mut connected_chains_rx) =
        mpsc::unbounded_channel::<ConnectedChainsChannel>();
    let (currencies_tx, mut currencies_rx) = mpsc::unbounded_channel::<CurrenciesChannel>();

    let connected_chains_jh = tokio::spawn(async move {
        while let Some((tx, (name, chain))) = connected_chains_rx.recv().await {
            tx.send(match connected_chains.entry(name) {
                Entry::Occupied(entry) => Some(entry.remove_entry()),
                Entry::Vacant(entry) => {
                    tracing::info!("Prepared the {:?} chain:\n{:#?}", entry.key(), chain);

                    entry.insert(chain);

                    None
                }
            })
            .unwrap();
        }

        connected_chains
    });

    let currencies_jh = tokio::spawn(async move {
        while let Some((tx, (name, currency))) = currencies_rx.recv().await {
            tx.send(match currencies.entry(name) {
                Entry::Occupied(entry) => Some(entry.remove_entry()),
                Entry::Vacant(entry) => {
                    tracing::info!(
                        %currency.chain_name, ?currency.asset_id,
                        "Registered the currency {:?}.",
                        entry.key(),
                    );

                    entry.insert(currency);

                    None
                }
            })
            .unwrap();
        }

        currencies
    });

    let (task_tracker, error_rx) = TaskTracker::new();

    for chain in chains {
        task_tracker.spawn(
            format!("the {:?} chain preparator", chain.name),
            prepare_chain(
                chain,
                connected_chains_tx.clone(),
                currencies_tx.clone(),
                account_lifetime,
                depth,
            ),
        );
    }

    drop((connected_chains_tx, currencies_tx));

    task_tracker.try_wait(error_rx).await?;

    Ok((connected_chains_jh.await?, currencies_jh.await?))
}

#[tracing::instrument(skip_all, fields(chain = chain.name))]
async fn prepare_chain(
    chain: Chain,
    connected_chains: UnboundedSender<ConnectedChainsChannel>,
    currencies: UnboundedSender<CurrenciesChannel>,
    account_lifetime: Timestamp,
    depth_option: Option<Timestamp>,
) -> Result<Cow<'static, str>> {
    let chain_name = chain.name;
    let endpoint = chain
        .endpoints
        .first()
        .context("chain doesn't have any `endpoints` in the config")?;
    let rpc_client = RpcClient::new(
        Client::builder()
            .build(endpoint.into())
            .await
            .context("failed to construct the RPC client")?,
    );

    let methods = LegacyRpcMethods::new(rpc_client.clone());
    let backend = Arc::new(LegacyBackend::builder().build(rpc_client.clone()));

    let genesis = methods
        .genesis_hash()
        .await
        .context("failed to fetch the genesis hash")?;
    let (finalized_number, finalized_hash) = fetch_finalized_head_number_and_hash(&methods).await?;
    let (metadata, runtime_version) = fetch_runtime(&methods, &*backend, finalized_hash).await?;

    let client = OnlineClient::from_backend_with(
        genesis,
        runtime_version,
        metadata.clone(),
        backend.clone(),
    )
    .context("failed to construct the API client")?;
    let constants = client.constants();

    let (block_time, runtime_api) = if metadata.pallet_by_name(BABE).is_some() {
        const EXPECTED_BLOCK_TIME: (&str, &str) = (BABE, "ExpectedBlockTime");

        (fetch_constant(&constants, EXPECTED_BLOCK_TIME)?, None)
    } else {
        const SLOT_DURATION: &str = "slot_duration";

        let runtime_api = client.runtime_api();

        (
            runtime_api
                .at(finalized_hash)
                .call(dynamic::runtime_api_call(
                    AURA,
                    SLOT_DURATION,
                    Vec::<u8>::new(),
                ))
                .await
                .context("failed to fetch Aura's slot duration")?
                .as_type()
                .context("failed to decode Aura's slot duration")?,
            Some(runtime_api),
        )
    };

    let block_time_non_zero =
        NonZeroU64::new(block_time).context("block interval can't equal 0")?;

    let account_lifetime_in_blocks = account_lifetime / block_time_non_zero;

    if account_lifetime_in_blocks == 0 {
        anyhow::bail!("block interval is longer than the given `account-lifetime`");
    }

    let depth_in_blocks = if let Some(depth) = depth_option {
        let depth_in_blocks = depth / block_time_non_zero;

        if depth_in_blocks > account_lifetime_in_blocks {
            anyhow::bail!("`depth` can't be greater than `account-lifetime`");
        }

        Some(
            NonZeroU64::new(depth_in_blocks)
                .context("block interval is longer than the given `depth`")?,
        )
    } else {
        None
    };

    let rpc = endpoint.into();

    let storage_client = client.storage();
    let storage = storage_client.at(finalized_hash);

    let assets_info_fetcher = if let Some(assets) = chain
        .asset
        .and_then(|mut assets| assets.pop().map(|latest| (latest, assets)))
    {
        const ASSET_ID: &str = "asset_id";
        const SOME: &str = "Some";

        let extension = metadata
            .extrinsic()
            .signed_extensions()
            .iter()
            .find(|extension| extension.identifier() == CHARGE_ASSET_TX_PAYMENT)
            .with_context(|| {
                format!("failed to find the {CHARGE_ASSET_TX_PAYMENT:?} extension in metadata")
            })?
            .extra_ty();
        let types = metadata.types();

        let TypeDef::Composite(ref extension_type) = types
            .resolve(extension)
            .with_context(|| {
                format!("failed to resolve the type of the {CHARGE_ASSET_TX_PAYMENT:?} extension")
            })?
            .type_def
        else {
            anyhow::bail!("{CHARGE_ASSET_TX_PAYMENT:?} extension has an unexpected type");
        };

        let asset_id_field = extension_type
            .fields
            .iter()
            .find_map(|field| {
                field
                    .name
                    .as_ref()
                    .and_then(|name| (name == ASSET_ID).then_some(field.ty.id))
            })
            .with_context(|| {
                format!(
                "failed to find the field {ASSET_ID:?} in the {CHARGE_ASSET_TX_PAYMENT:?} extension"
            )
            })?;

        let TypeDef::Variant(ref option) = types.resolve(asset_id_field).with_context(|| {
            format!(
                "failed to resolve the type of the field {ASSET_ID:?} in the {CHARGE_ASSET_TX_PAYMENT:?} extension"
            )
        })?.type_def else {
            anyhow::bail!(
                "field {ASSET_ID:?} in the {CHARGE_ASSET_TX_PAYMENT:?} extension has an unexpected type"
            );
        };

        let asset_id_some = option.variants.iter().find_map(|variant| {
            if variant.name == SOME {
                variant.fields.first().map(|field| {
                    if variant.fields.len() > 1 {
                        tracing::warn!(
                            ?variant.fields,
                            "The field {ASSET_ID:?} in the {CHARGE_ASSET_TX_PAYMENT:?} extension contains multiple inner fields instead of just 1."
                        );
                    }

                    field.ty.id
                })
            } else {
                None
            }
        }).with_context(|| format!(
            "field {ASSET_ID:?} in the {CHARGE_ASSET_TX_PAYMENT:?} extension doesn't contain the {SOME:?} variant"
        ))?;

        let asset_id = &types.resolve(asset_id_some).with_context(|| {
            format!(
                "failed to resolve the type of the {SOME:?} variant of the field {ASSET_ID:?} in the {CHARGE_ASSET_TX_PAYMENT:?} extension"
            )
        })?.type_def;

        let pallet_index = if let TypeDef::Primitive(_) = asset_id {
            None
        } else {
            Some(metadata.pallet_by_name_err(ASSETS)?.index())
        };
        Some(AssetsInfoFetcher {
            assets,
            storage: &storage,
            pallet_index,
        })
    } else {
        None
    };

    let storage = if assets_info_fetcher.is_some() {
        Some(storage_client)
    } else {
        None
    };

    let properties = ChainProperties::fetch(
        &chain_name,
        currencies,
        &constants,
        chain.native_token,
        assets_info_fetcher,
        account_lifetime_in_blocks,
        depth_in_blocks,
    )
    .await?;

    let connected_chain = ConnectedChain {
        methods,
        genesis,
        rpc,
        client,
        storage,
        properties,
        constants,
        runtime_api,
        backend,
    };

    let (tx, rx) = oneshot::channel();

    connected_chains
        .send((tx, (chain_name, connected_chain)))
        .unwrap();

    if let Some((name, _)) = rx.await.unwrap() {
        anyhow::bail!(
            "found `[chain]`s with the same name ({name:?}) in the config, all chain names must be unique",
        );
    }

    Ok("".into())
}

#[derive(Debug)]
pub struct Currency {
    chain: String,
    asset: Option<AssetId>,
}

pub struct ConnectedChain {
    rpc: String,
    methods: LegacyRpcMethods<RuntimeConfig>,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
    client: OnlineClient,
    genesis: BlockHash,
    properties: ChainProperties,
    constants: ConstantsClient<RuntimeConfig, OnlineClient>,
    storage: Option<StorageClient<RuntimeConfig, OnlineClient>>,
    runtime_api: Option<RuntimeApiClient<RuntimeConfig, OnlineClient>>,
}

impl Debug for ConnectedChain {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct(stringify!(ConnectedChain))
            .field("rpc", &self.rpc)
            .field("genesis", &self.genesis)
            .field("properties", &self.properties)
            .finish_non_exhaustive()
    }
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

pub struct Processor {
    state: State,
    recipient: AccountId,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
    shutdown_notification: CancellationToken,
    methods: LegacyRpcMethods<RuntimeConfig>,
    client: OnlineClient,
    storage: StorageClient<RuntimeConfig, OnlineClient>,
}

impl Processor {
    pub async fn ignite(
        rpc: String,
        recipient: AccountId,
        state: State,
        notif: CancellationToken,
    ) -> Result<Cow<'static, str>> {
        let client = Client::builder().build(rpc.clone()).await.unwrap();
        let rpc_c = RpcClient::new(client);
        let methods = LegacyRpcMethods::new(rpc_c.clone());
        let backend = Arc::new(LegacyBackend::builder().build(rpc_c));
        let onl = OnlineClient::from_backend(backend.clone()).await.unwrap();
        let st = onl.storage();

        Processor {
            state,
            recipient,
            backend,
            shutdown_notification: notif,
            methods,
            client: onl,
            storage: st,
        }
        .execute()
        .await
        .or_else(|error| {
            error
                .downcast()
                .map(|Shutdown| "The RPC module is shut down.".into())
        })
    }

    async fn execute(mut self) -> Result<Cow<'static, str>> {
        let (head_number, head_hash) = self
            .finalized_head_number_and_hash()
            .await
            .context("failed to get the chain head")?;

        let mut next_unscanned_number;
        let mut subscription;

        next_unscanned_number = head_number.checked_add(1).context(MAX_BLOCK_NUMBER_ERROR)?;
        subscription = self.finalized_heads().await?;

        loop {
            self.process_finalized_heads(subscription, &mut next_unscanned_number)
                .await?;

            tracing::warn!("Lost the connection while processing finalized heads. Retrying...");

            subscription = self
                .finalized_heads()
                .await
                .context("failed to update the subscription while processing finalized heads")?;
        }
    }

    async fn finalized_head_number_and_hash(&self) -> Result<(BlockNumber, BlockHash)> {
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
            if self.shutdown_notification.is_cancelled() {
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
                () = self.shutdown_notification.cancelled() => {
                    return Err(Shutdown.into());
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

    async fn process_block(&self, number: BlockNumber, hash: BlockHash) -> Result<()> {
        tracing::debug!("Processing the block: {number}.");

        let block = self
            .client
            .blocks()
            .at(hash)
            .await
            .context("failed to obtain a block for processing")?;
        let events = block
            .events()
            .await
            .context("failed to obtain block events")?;

        //let invoices = &mut *self.state.invoices.write().await;

        // let mut update = false;
        // let mut invoices_changes = HashMap::new();

        for event_result in events.iter() {
            const UPDATE: &str = "CodeUpdated";
            const TRANSFERRED: &str = "Transferred";
            let event = event_result.context("failed to decode an event")?;
            let metadata = event.event_metadata();

            #[allow(clippy::single_match)]
            match (metadata.pallet.name(), &*metadata.variant.name) {
                // (SYSTEM, UPDATE) => update = true,
                (ASSETS, TRANSFERRED) => {
                    let tr = Transferred::deserialize(
                        event
                            .field_values()
                            .context("failed to decode event's fields")?,
                    )
                    .context("failed to deserialize a transfer event")?;

                    tracing::info!("{tr:?}");
                    /* TODO process using cache and db access
                    #[allow(clippy::unnecessary_find_map)]
                    if let Some(invoic) = invoices.iter().find_map(|invoic| {
                        tracing::info!("{tr:?} {invoic:?}");
                        tracing::info!("{}", tr.to == invoic.1.paym_acc);
                        tracing::info!("{}", *invoic.1.amount >= tr.amount);

                        if tr.to == invoic.1.paym_acc && *invoic.1.amount <= tr.amount {
                            Some(invoic)
                        } else {
                            None
                        }
                    }) {
                        tracing::info!("{invoic:?}");

                        if !invoic.1.callback.is_empty() {
                            tracing::info!("{:?}", invoic.1.callback);

                            crate::callback::callback(
                                invoic.1.callback.clone(),
                                invoic.0.to_string(),
                                self.state.recipient.clone(),
                                self.state.debug,
                                self.state.remark.clone(),
                                invoic.1.amount,
                                self.state.rpc.clone(),
                                invoic.1.paym_acc.clone(),
                            )
                            .await;
                        }

                        invoices.insert(
                            invoic.0.clone(),
                            Invoicee {
                                callback: invoic.1.callback.clone(),
                                amount: Balance(*invoic.1.amount),
                                paid: true,
                                paym_acc: invoic.1.paym_acc.clone(),
                            },
                        );
                    }*/
                }
                _ => {}
            }
        }

        // for (invoice, changes) in invoices_changes {
        //     let price = match changes.invoice.status {
        //         InvoiceStatus::Unpaid(price) | InvoiceStatus::Paid(price) => price,
        //     };

        //     self.process_unpaid(&block, changes, hash, invoice, price)
        //         .await
        //         .context("failed to process an unpaid invoice")?;
        // }

        Ok(())
    }

    async fn balance(&self, hash: BlockHash, account: &AccountId) -> Result<Balance> {
        const ACCOUNT: &str = "Account";
        const BALANCE: &str = "balance";

        if let Some(account_info) = self
            .storage
            .at(hash)
            .fetch(&dynamic::storage(
                ASSETS,
                ACCOUNT,
                vec![
                    Value::from(1337u32),
                    Value::from_bytes(AsRef::<[u8; 32]>::as_ref(account)),
                ],
            ))
            .await
            .context("failed to fetch account info from the chain")?
        {
            let decoded_account_info = account_info
                .to_value()
                .context("failed to decode account info")?;
            let encoded_balance = decoded_account_info
                .at(BALANCE)
                .with_context(|| format!("{BALANCE} field wasn't found in account info"))?;

            encoded_balance.as_u128().map(Balance).with_context(|| {
                format!("expected `u128` as the type of a balance, got {encoded_balance}")
            })
        } else {
            Ok(Balance(0))
        }
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
        let extensions = DefaultExtrinsicParamsBuilder::new()
            .mortal_unchecked(number.into(), hash, block_hash_count.into())
            .tip_of(0, Asset::Id(1337));

        self.client
            .tx()
            .create_signed(&call, signer, extensions.build())
            .await
            .context("failed to create a transfer transaction")
    }

    // async fn current_nonce(&self, account: &AccountId) -> Result<Nonce> {
    //     self.api
    //         .blocks
    //         .at(self.finalized_head_number_and_hash().await?.0)
    //         .await
    //         .context("failed to obtain the best block for fetching an account nonce")?
    //         .account_nonce(account)
    //         .await
    //         .context("failed to fetch an account nonce by the API client")
    // }

    // async fn process_unpaid(
    //     &self,
    //     block: &Block<RuntimeConfig, OnlineClient>,
    //     mut changes: InvoiceChanges,
    //     hash: BlockHash,
    //     invoice: AccountId,
    //     price: Balance,
    // ) -> Result<()> {
    //     let balance = self.balance(hash, &invoice).await?;

    //     if let Some(_remaining) = balance.checked_sub(*price) {
    //         changes.invoice.status = InvoiceStatus::Paid(price);

    //         let block_nonce = block
    //             .account_nonce(&invoice)
    //             .await
    //             .context(BLOCK_NONCE_ERROR)?;
    //         let current_nonce = self.current_nonce(&invoice).await?;

    //         if current_nonce <= block_nonce {
    //             let properties = self.database.properties().await;
    //             let block_hash_count = properties.block_hash_count;
    //             let signer = changes.invoice.signer(self.database.pair())?;

    //             let transfers = vec![construct_transfer(
    //                 &changes.invoice.recipient,
    //                 price - EXPECTED_USDX_FEE,
    //                 self.database.properties().await.usd_asset,
    //             )];
    //             let tx = self
    //                 .batch_transfer(current_nonce, block_hash_count, &signer, transfers.clone())
    //                 .await?;

    //             self.methods
    //                 .author_submit_extrinsic(tx.encoded())
    //                 .await
    //                 .context("failed to submit an extrinsic")
    //                 .unwrap();
    //         }
    //     }

    //     Ok(())
    // }
}

fn construct_transfer(to: &AccountId, amount: u128) -> Value {
    const TRANSFER_KEEP_ALIVE: &str = "transfer";

    dbg!(amount);

    dynamic::tx(
        ASSETS,
        TRANSFER_KEEP_ALIVE,
        vec![
            1337.into(),
            scale_value::value!(Id(Value::from_bytes(to))),
            amount.into(),
        ],
    )
    .into_value()
}

#[derive(Debug)]
struct InvoiceChanges {
    invoice: Invoicee,
    incoming: HashMap<AccountId, Balance>,
}

#[derive(Deserialize, Debug)]
struct Transferred {
    asset_id: u32,
    // The implementation of `Deserialize` for `AccountId32` works only with strings.
    #[serde(deserialize_with = "account_deserializer")]
    from: AccountId,
    #[serde(deserialize_with = "account_deserializer")]
    to: AccountId,
    amount: u128,
}

fn account_deserializer<'de, D>(deserializer: D) -> Result<AccountId, D::Error>
where
    D: Deserializer<'de>,
{
    <([u8; 32],)>::deserialize(deserializer).map(|address| AccountId::new(address.0))
}

// impl Transferred {
//     fn process(
//         self,
//         invoices_changes: &mut HashMap<AccountId, InvoiceChanges>,
//         invoices: &mut HashMap<String, Invoicee>,
//     ) -> Result<()> {
//         let usd_asset = 1337u32;

//         tracing::debug!("Transferred event: {self:?}");

//         if self.from == self.to || self.amount == 0 || self.asset_id != usd_asset {
//             return Ok(());
//         }

//         match invoices_changes.entry(self.to) {
//             Entry::Occupied(entry) => {
//                 entry
//                     .into_mut()
//                     .incoming
//                     .entry(self.from)
//                     .and_modify(|amount| *amount = Balance(amount.saturating_add(self.amount)))
//                     .or_insert(Balance(self.amount));
//             }
//             Entry::Vacant(entry) => {
//                 if let (None, Some(encoded_invoice)) =
//                     (invoices.get(&self.from)?, invoices.get(entry.key())?)
//                 {
//                     entry.insert(InvoiceChanges {
//                         invoice: encoded_invoice.value(),
//                         incoming: [(self.from, self.amount)].into(),
//                     });
//                 }
//             }
//         }

//         Ok(())
//     }
// }
