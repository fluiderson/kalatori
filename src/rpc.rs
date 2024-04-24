use crate::{
    asset::Asset,
    database::{Invoicee, State},
    server::{
        CurrencyInfo, OrderInfo, OrderStatus, PaymentStatus, ServerInfo, TokenKind,
        WithdrawalStatus,
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
    dynamic::{self, At, DecodedValueThunk, Value},
    error::{MetadataError, RpcError},
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

// Pallets

const SYSTEM: &str = "System";
const BALANCES: &str = "Balances";
const UTILITY: &str = "Utility";
const ASSETS: &str = "Assets";
const BABE: &str = "Babe";

type ConnectedChainsChannel = (Sender<bool>, (Arc<String>, ConnectedChain));
type CurrenciesChannel = (Sender<Option<(String, Currency)>>, (String, Currency));

// Rather used to check whether the `ChargeAssetTxPayment` has `MultiLocation` in the asset ID type.
fn fetch_assets_pallet_index(metadata: &Metadata) -> Result<Option<PalletIndex>> {
    const ASSET_ID: &str = "asset_id";
    const SOME: &str = "Some";
    const CHARGE_ASSET_TX_PAYMENT: &str = "ChargeAssetTxPayment";

    let extension = metadata
        .extrinsic()
        .signed_extensions()
        .iter()
        .find(|extension| extension.identifier() == CHARGE_ASSET_TX_PAYMENT)
        .with_context(|| {
            format!("failed to find the `{CHARGE_ASSET_TX_PAYMENT}` extension in metadata",)
        })?
        .extra_ty();
    let types = metadata.types();

    let extension_type = match &types
        .resolve(extension)
        .with_context(|| {
            format!("failed to resolve the type of the `{CHARGE_ASSET_TX_PAYMENT}` extension",)
        })?
        .type_def
    {
        TypeDef::Composite(extension_type) => extension_type,
        other => anyhow::bail!(
            "`{CHARGE_ASSET_TX_PAYMENT}` extension has an unexpected type (`{other:?}`)",
        ),
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
                "failed to find the field {ASSET_ID:?} in the `{CHARGE_ASSET_TX_PAYMENT}` extension",
            )
        })?;

    let option = match &types
        .resolve(asset_id_field)
        .with_context(|| {
            format!(
                "failed to resolve the type of the field {ASSET_ID:?} in the `{CHARGE_ASSET_TX_PAYMENT}` extension",
            )
        })?
        .type_def
    {
        TypeDef::Variant(option) => option,
        other => anyhow::bail!(
            "field {ASSET_ID:?} in the `{CHARGE_ASSET_TX_PAYMENT}` extension has an unexpected type (`{other:?}`)",
        ),
    };

    let asset_id_some = option
        .variants
        .iter()
        .find_map(|variant| {
            if variant.name == SOME {
                variant.fields.first().map(|field| field.ty.id)
            } else {
                None
            }
        })
        .with_context(|| {
            format!(
                "field {ASSET_ID:?} in the `{CHARGE_ASSET_TX_PAYMENT}` extension doesn't contain the `{SOME}` variant",
            )
        })?;

    let asset_id = &types.resolve(asset_id_some).with_context(|| {
        format!(
            "failed to resolve the type of the {SOME:?} variant of the field {ASSET_ID:?} in the `{CHARGE_ASSET_TX_PAYMENT}` extension",
        )
    })?.type_def;

    Ok(if matches!(asset_id, TypeDef::Primitive(_)) {
        None
    } else {
        Some(metadata.pallet_by_name_err(ASSETS)?.index())
    })
}

async fn fetch_finalized_head_hash(methods: &LegacyRpcMethods<RuntimeConfig>) -> Result<BlockHash> {
    methods
        .chain_get_finalized_head()
        .await
        .context("failed to get the finalized head hash")
}

// async fn fetch_finalized_head_number_and_hash(
//     methods: &LegacyRpcMethods<RuntimeConfig>,
// ) -> Result<(BlockNumber, BlockHash)> {
//     let head_hash = methods
//         .chain_get_finalized_head()
//         .await
//         .context("failed to get the finalized head hash")?;
//     let head = methods
//         .chain_get_block(Some(head_hash))
//         .await
//         .context("failed to get the finalized head")?
//         .context("received nothing after requesting the finalized head")?;

//     Ok((head.block.header.number, head_hash))
// }

async fn fetch_runtime(
    methods: &LegacyRpcMethods<RuntimeConfig>,
    backend: &impl Backend<RuntimeConfig>,
    at: BlockHash,
) -> Result<((Metadata, bool), RuntimeVersion)> {
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

// Also returns whether the metadata was fetched by the legacy method.
async fn fetch_metadata(
    backend: &impl Backend<RuntimeConfig>,
    at: BlockHash,
) -> Result<(Metadata, bool)> {
    const LATEST_SUPPORTED_METADATA_VERSION: u32 = 15;

    backend
        .metadata_at_version(LATEST_SUPPORTED_METADATA_VERSION, at)
        .map_ok(|metadata| (metadata, false))
        .or_else(|error| async {
            if matches!(
                error,
                subxt::Error::Rpc(RpcError::ClientError(_)) | subxt::Error::Other(_)
            ) {
                backend
                    .legacy_metadata(at)
                    .map_ok(|metadata| (metadata, true))
                    .await
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
struct AssetsPallet {
    multi_location: Option<PalletIndex>,
    assets: HashMap<AssetId, AssetProperties>,
}

#[derive(Debug)]
struct AssetProperties {
    min_balance: Balance,
    decimals: Decimals,
}

async fn try_add_asset(
    assets: &mut HashMap<AssetId, AssetProperties>,
    id: AssetId,
    storage: &Storage<RuntimeConfig, OnlineClient>,
) -> Result<()> {
    match assets.entry(id) {
        Entry::Occupied(_) => Err(anyhow::anyhow!(
            "chain has 2 assets with the same ID ({id})"
        )),
        Entry::Vacant(entry) => {
            entry.insert(AssetProperties {
                min_balance: check_sufficiency_and_fetch_min_balance(storage, id).await?,
                decimals: fetch_asset_decimals(storage, id).await?,
            });

            Ok(())
        }
    }
}

#[derive(Debug)]
struct ChainProperties {
    address_format: Ss58AddressFormat,
    existential_deposit: Option<Balance>,
    assets_pallet: Option<AssetsPallet>,
    block_hash_count: BlockNumber,
    account_lifetime: BlockNumber,
}

impl ChainProperties {
    #[allow(clippy::too_many_arguments)]
    async fn fetch(
        chain: Arc<String>,
        currencies: UnboundedSender<CurrenciesChannel>,
        storage_client: &StorageClient<RuntimeConfig, OnlineClient>,
        finalized_hash: BlockHash,
        metadata: &Metadata,
        constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
        native_token_option: Option<NativeToken>,
        assets_option: Option<Vec<AssetInfo>>,
        account_lifetime: BlockNumber,
    ) -> Result<(Self, Option<Decimals>)> {
        const ADDRESS_PREFIX: (&str, &str) = (SYSTEM, "SS58Prefix");
        const EXISTENTIAL_DEPOSIT: (&str, &str) = (BALANCES, "ExistentialDeposit");
        const BLOCK_HASH_COUNT: (&str, &str) = (SYSTEM, "BlockHashCount");

        let chain_clone = chain.clone();

        let try_add_currency = |name, asset| async move {
            let (tx, rx) = oneshot::channel();

            currencies
                .send((
                    tx,
                    (
                        name,
                        Currency {
                            chain: chain_clone,
                            asset,
                        },
                    ),
                ))
                .unwrap();

            if let Some((
                same_name,
                Currency {
                    chain: other_chain, ..
                },
            )) = rx.await.unwrap()
            {
                Err(anyhow::anyhow!(
                        "chain {other_chain:?} already has the native token or an asset with the name {same_name:?}, all currency names must be unique"
                    ))
            } else {
                Ok(())
            }
        };

        let assets_pallet = if let Some((last_asset_info, assets_info)) =
            assets_option.and_then(|mut assets| assets.pop().map(|last| (last, assets)))
        {
            let mut assets = HashMap::with_capacity(assets_info.len().saturating_add(1));
            let storage = storage_client.at(finalized_hash);

            for asset_info in assets_info {
                try_add_currency.clone()(asset_info.name, Some(asset_info.id)).await?;
                try_add_asset(&mut assets, asset_info.id, &storage).await?;
            }

            try_add_currency.clone()(last_asset_info.name, Some(last_asset_info.id)).await?;
            try_add_asset(&mut assets, last_asset_info.id, &storage).await?;

            Some(AssetsPallet {
                multi_location: fetch_assets_pallet_index(metadata)?,
                assets,
            })
        } else {
            None
        };

        let address_format = Ss58AddressFormat::custom(fetch_constant(constants, ADDRESS_PREFIX)?);
        let block_hash_count = fetch_constant(constants, BLOCK_HASH_COUNT)?;

        Ok(if let Some(native_token) = native_token_option {
            try_add_currency(native_token.native_token, None).await?;

            (
                Self {
                    address_format,
                    existential_deposit: Some(
                        fetch_constant(constants, EXISTENTIAL_DEPOSIT).map(Balance)?,
                    ),
                    assets_pallet,
                    block_hash_count,
                    account_lifetime,
                },
                Some(native_token.decimals),
            )
        } else {
            (
                Self {
                    address_format,
                    existential_deposit: None,
                    assets_pallet,
                    block_hash_count,
                    account_lifetime,
                },
                None,
            )
        })
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
        format!("asset {asset} {DECIMALS:?} must be less than 256, got {decimals}")
    })
}

pub async fn prepare(
    chains: Vec<Chain>,
    account_lifetime: Timestamp,
) -> Result<(HashMap<Arc<String>, ConnectedChain>, HashMap<String, Currency>)> {
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
                Entry::Occupied(_) => true,
                Entry::Vacant(entry) => {
                    tracing::info!("Prepared the {:?} chain:\n{:#?}", entry.key(), chain);

                    entry.insert(chain);

                    false
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
                        %currency.chain, ?currency.asset,
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
            ),
        );
    }

    drop((connected_chains_tx, currencies_tx));

    task_tracker.try_wait(error_rx).await?;

    Ok((connected_chains_jh.await?, currencies_jh.await?))
}

async fn fetch_block_time(
    constants: &ConstantsClient<RuntimeConfig, OnlineClient>,
    client: &OnlineClient,
    finalized_hash: BlockHash,
    is_metadata_legacy: bool,
    metadata: &Metadata,
) -> Result<Timestamp> {
    const EXPECTED_BLOCK_TIME: (&str, &str) = (BABE, "ExpectedBlockTime");
    const MOONBEAM_BLOCK_TIME: (&str, &str) = ("ParachainStaking", "BlockTime");
    const AURA: &str = "AuraApi";
    const SLOT_DURATION: &str = "slot_duration";
    const AURA_SLOT_DURATION: &str = "AuraApi_slot_duration";

    let error_matches = |error: anyhow::Error| {
        if matches!(
            error.downcast_ref(),
            Some(subxt::Error::Metadata(MetadataError::PalletNameNotFound(_) | MetadataError::ConstantNameNotFound(_)))
        ) {
            Ok(())
        } else {
            Err(error)
        }
    };

    match fetch_constant(constants, EXPECTED_BLOCK_TIME) {
        Ok(block_time) => return Ok(block_time),
        Err(error) => error_matches(error)?,
    }

    match fetch_constant(constants, MOONBEAM_BLOCK_TIME) {
        Ok(block_time) => return Ok(block_time),
        Err(error) => error_matches(error)?,
    }

    let runtime_api = client.runtime_api().at(finalized_hash);

    match runtime_api
        .call(dynamic::runtime_api_call(
            AURA,
            SLOT_DURATION,
            Vec::<u8>::new(),
        ))
        .await
    {
        Ok(encoded_block_time) => {
            return encoded_block_time
                .as_type()
                .context("failed to decode Aura's slot duration");
        }
        Err(error) => {
            if !matches!(
                error,
                subxt::Error::Metadata(MetadataError::RuntimeTraitNotFound(_))
            ) & is_metadata_legacy
                && metadata.runtime_api_traits().len() == 0
            {
                return Err(error).context("failed to fetch Aura's slot duration");
            }
        }
    }

    tracing::warn!("Chain metadata was fetched by the legacy method & doesn't contain runtime API traits metadata.");

    runtime_api
        .call_raw(AURA_SLOT_DURATION, None)
        .await
        .context("failed to fetch & decode Aura's slot duration")
}

#[tracing::instrument(skip_all, fields(chain = chain.name))]
async fn prepare_chain(
    chain: Chain,
    connected_chains: UnboundedSender<ConnectedChainsChannel>,
    currencies: UnboundedSender<CurrenciesChannel>,
    account_lifetime: Timestamp,
) -> Result<Cow<'static, str>> {
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
    let finalized_hash = fetch_finalized_head_hash(&methods).await?;
    let ((metadata, is_metadata_legacy), runtime_version) =
        fetch_runtime(&methods, &*backend, finalized_hash).await?;

    let client = OnlineClient::from_backend_with(
        genesis,
        runtime_version,
        metadata.clone(),
        backend.clone(),
    )
    .context("failed to construct the API client")?;
    let constants = client.constants();

    let block_time = fetch_block_time(
        &constants,
        &client,
        finalized_hash,
        is_metadata_legacy,
        &metadata,
    )
    .await?;
    let account_lifetime_in_blocks = account_lifetime
        .checked_div(block_time)
        .context("block interval can't be 0")?;

    if account_lifetime_in_blocks == 0 {
        anyhow::bail!("block interval is longer than the given `account-lifetime`");
    }

    let rpc = endpoint.into();
    let storage = client.storage();
    let chain_name: Arc<String> = chain.name.into();
    let (properties, decimals) = ChainProperties::fetch(
        chain_name.clone(),
        currencies,
        &storage,
        finalized_hash,
        &metadata,
        &constants,
        chain.native_token,
        chain.asset,
        account_lifetime,
    )
    .await?;
    let (tx, rx) = oneshot::channel();

    if properties.existential_deposit.is_none() & properties.assets_pallet.is_none() {
        tracing::warn!("A chain doesn't have native nor asset tokens set in the config.");
    }

    connected_chains
        .send((
            tx,
            (
                chain_name,
                ConnectedChain {
                    rpc,
                    methods,
                    backend,
                    client,
                    genesis,
                    properties,
                    constants,
                    storage,
                    decimals,
                },
            ),
        ))
        .unwrap();

    if rx.await.unwrap() {
        anyhow::bail!(
            "found `[chain]`s with the same name in the config, all chain names must be unique",
        );
    }

    Ok("".into())
}

#[derive(Debug)]
pub struct Currency {
    chain: Arc<String>,
    asset: Option<AssetId>,
}

pub struct ConnectedChain {
    rpc: String,
    methods: LegacyRpcMethods<RuntimeConfig>,
    backend: Arc<LegacyBackend<RuntimeConfig>>,
    client: OnlineClient,
    pub genesis: BlockHash,
    properties: ChainProperties,
    constants: ConstantsClient<RuntimeConfig, OnlineClient>,
    storage: StorageClient<RuntimeConfig, OnlineClient>,
    decimals: Option<Decimals>,
}

impl Debug for ConnectedChain {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct(stringify!(ConnectedChain))
            .field("rpc", &self.rpc)
            .field("genesis", &self.genesis)
            .field("properties", &self.properties)
            .field("decimals", &self.decimals)
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

// pub struct Processor {
//     state: Arc<State>,
//     backend: Arc<LegacyBackend<RuntimeConfig>>,
//     shutdown_notification: CancellationToken,
//     methods: LegacyRpcMethods<RuntimeConfig>,
//     client: OnlineClient,
//     storage: StorageClient<RuntimeConfig, OnlineClient>,
// }

// impl Processor {
//     pub async fn ignite(state: Arc<State>, notif: CancellationToken) -> Result<Cow<'static, str>> {
//         let client = Client::builder().build(state.rpc.clone()).await.unwrap();
//         let rpc_c = RpcClient::new(client);
//         let methods = LegacyRpcMethods::new(rpc_c.clone());
//         let backend = Arc::new(LegacyBackend::builder().build(rpc_c));
//         let onl = OnlineClient::from_backend(backend.clone()).await.unwrap();
//         let st = onl.storage();

//         Processor {
//             state,
//             backend,
//             shutdown_notification: notif,
//             methods,
//             client: onl,
//             storage: st,
//         }
//         .execute()
//         .await
//         .or_else(|error| {
//             error
//                 .downcast()
//                 .map(|Shutdown| "The RPC module is shut down.".into())
//         })
//     }

//     async fn execute(mut self) -> Result<Cow<'static, str>> {
//         let (head_number, head_hash) = self
//             .finalized_head_number_and_hash()
//             .await
//             .context("failed to get the chain head")?;

//         let mut next_unscanned_number;
//         let mut subscription;

//         next_unscanned_number = head_number.checked_add(1).context(MAX_BLOCK_NUMBER_ERROR)?;
//         subscription = self.finalized_heads().await?;

//         loop {
//             self.process_finalized_heads(subscription, &mut next_unscanned_number)
//                 .await?;

//             tracing::warn!("Lost the connection while processing finalized heads. Retrying...");

//             subscription = self
//                 .finalized_heads()
//                 .await
//                 .context("failed to update the subscription while processing finalized heads")?;
//         }
//     }

//     async fn finalized_head_number_and_hash(&self) -> Result<(BlockNumber, BlockHash)> {
//         let head_hash = self
//             .methods
//             .chain_get_finalized_head()
//             .await
//             .context("failed to get the finalized head hash")?;
//         let head = self
//             .methods
//             .chain_get_block(Some(head_hash))
//             .await
//             .context("failed to get the finalized head")?
//             .context("received nothing after requesting the finalized head")?;

//         Ok((head.block.header.number, head_hash))
//     }

//     async fn finalized_heads(&self) -> Result<RpcSubscription<<RuntimeConfig as Config>::Header>> {
//         self.methods
//             .chain_subscribe_finalized_heads()
//             .await
//             .context("failed to subscribe to finalized heads")
//     }

//     async fn process_skipped(
//         &self,
//         next_unscanned: &mut BlockNumber,
//         head: BlockNumber,
//     ) -> Result<()> {
//         for skipped_number in *next_unscanned..head {
//             if self.shutdown_notification.is_cancelled() {
//                 return Err(Shutdown.into());
//             }

//             let skipped_hash = self
//                 .methods
//                 .chain_get_block_hash(Some(skipped_number.into()))
//                 .await
//                 .context("failed to get the hash of a skipped block")?
//                 .context("received nothing after requesting the hash of a skipped block")?;

//             self.process_block(skipped_number, skipped_hash).await?;
//         }

//         *next_unscanned = head;

//         Ok(())
//     }

//     async fn process_finalized_heads(
//         &mut self,
//         mut subscription: RpcSubscription<<RuntimeConfig as Config>::Header>,
//         next_unscanned: &mut BlockNumber,
//     ) -> Result<()> {
//         loop {
//             tokio::select! {
//                 biased;
//                 () = self.shutdown_notification.cancelled() => {
//                     return Err(Shutdown.into());
//                 }
//                 head_result_option = subscription.next() => {
//                     if let Some(head_result) = head_result_option {
//                         let head = head_result.context(
//                             "received an error from the RPC client while processing finalized heads"
//                         )?;

//                         self
//                             .process_skipped(next_unscanned, head.number)
//                             .await
//                             .context("failed to process a skipped gap in the listening mode")?;
//                         self.process_block(head.number, head.hash()).await?;

//                         *next_unscanned = head.number
//                             .checked_add(1)
//                             .context(MAX_BLOCK_NUMBER_ERROR)?;
//                     } else {
//                         break;
//                     }
//                 }
//             }
//         }

//         Ok(())
//     }

//     async fn process_block(&self, number: BlockNumber, hash: BlockHash) -> Result<()> {
//         tracing::debug!("Processing the block: {number}.");

//         let block = self
//             .client
//             .blocks()
//             .at(hash)
//             .await
//             .context("failed to obtain a block for processing")?;
//         let events = block
//             .events()
//             .await
//             .context("failed to obtain block events")?;

//         let invoices = &mut *self.state.invoices.write().await;

//         // let mut update = false;
//         // let mut invoices_changes = HashMap::new();

//         for event_result in events.iter() {
//             const UPDATE: &str = "CodeUpdated";
//             const TRANSFERRED: &str = "Transferred";
//             let event = event_result.context("failed to decode an event")?;
//             let metadata = event.event_metadata();

//             #[allow(clippy::single_match)]
//             match (metadata.pallet.name(), &*metadata.variant.name) {
//                 // (SYSTEM, UPDATE) => update = true,
//                 (ASSETS, TRANSFERRED) => {
//                     let tr = Transferred::deserialize(
//                         event
//                             .field_values()
//                             .context("failed to decode event's fields")?,
//                     )
//                     .context("failed to deserialize a transfer event")?;

//                     tracing::info!("{tr:?}");

//                     #[allow(clippy::unnecessary_find_map)]
//                     if let Some(invoic) = invoices.iter().find_map(|invoic| {
//                         tracing::info!("{tr:?} {invoic:?}");
//                         tracing::info!("{}", tr.to == invoic.1.paym_acc);
//                         tracing::info!("{}", *invoic.1.amount >= tr.amount);

//                         if tr.to == invoic.1.paym_acc && *invoic.1.amount <= tr.amount {
//                             Some(invoic)
//                         } else {
//                             None
//                         }
//                     }) {
//                         tracing::info!("{invoic:?}");

//                         if !invoic.1.callback.is_empty() {
//                             tracing::info!("{:?}", invoic.1.callback);

//                             crate::callback::callback(
//                                 invoic.1.callback.clone(),
//                                 invoic.0.to_string(),
//                                 self.state.recipient.clone(),
//                                 self.state.debug,
//                                 self.state.remark.clone(),
//                                 invoic.1.amount,
//                                 self.state.rpc.clone(),
//                                 invoic.1.paym_acc.clone(),
//                             )
//                             .await;
//                         }

//                         invoices.insert(
//                             invoic.0.clone(),
//                             Invoicee {
//                                 callback: invoic.1.callback.clone(),
//                                 amount: Balance(*invoic.1.amount),
//                                 paid: true,
//                                 paym_acc: invoic.1.paym_acc.clone(),
//                             },
//                         );
//                     }
//                 }
//                 _ => {}
//             }
//         }

//         // for (invoice, changes) in invoices_changes {
//         //     let price = match changes.invoice.status {
//         //         InvoiceStatus::Unpaid(price) | InvoiceStatus::Paid(price) => price,
//         //     };

//         //     self.process_unpaid(&block, changes, hash, invoice, price)
//         //         .await
//         //         .context("failed to process an unpaid invoice")?;
//         // }

//         Ok(())
//     }

//     async fn balance(&self, hash: BlockHash, account: &AccountId) -> Result<Balance> {
//         const ACCOUNT: &str = "Account";
//         const BALANCE: &str = "balance";

//         if let Some(account_info) = self
//             .storage
//             .at(hash)
//             .fetch(&dynamic::storage(
//                 ASSETS,
//                 ACCOUNT,
//                 vec![
//                     Value::from(1337u32),
//                     Value::from_bytes(AsRef::<[u8; 32]>::as_ref(account)),
//                 ],
//             ))
//             .await
//             .context("failed to fetch account info from the chain")?
//         {
//             let decoded_account_info = account_info
//                 .to_value()
//                 .context("failed to decode account info")?;
//             let encoded_balance = decoded_account_info
//                 .at(BALANCE)
//                 .with_context(|| format!("{BALANCE} field wasn't found in account info"))?;

//             encoded_balance.as_u128().map(Balance).with_context(|| {
//                 format!("expected `u128` as the type of a balance, got {encoded_balance}")
//             })
//         } else {
//             Ok(Balance(0))
//         }
//     }

//     async fn batch_transfer(
//         &self,
//         nonce: Nonce,
//         block_hash_count: BlockNumber,
//         signer: &PairSigner<RuntimeConfig, Pair>,
//         transfers: Vec<Value>,
//     ) -> Result<SubmittableExtrinsic<RuntimeConfig, OnlineClient>> {
//         const FORCE_BATCH: &str = "force_batch";

//         let call = dynamic::tx(UTILITY, FORCE_BATCH, vec![Value::from(transfers)]);
//         let (number, hash) = self
//             .finalized_head_number_and_hash()
//             .await
//             .context("failed to get the chain head while constructing a transaction")?;
//         let extensions = DefaultExtrinsicParamsBuilder::new()
//             .mortal_unchecked(number.into(), hash, block_hash_count.into())
//             .tip_of(0, Asset::Id(1337));

//         self.client
//             .tx()
//             .create_signed(&call, signer, extensions.build())
//             .await
//             .context("failed to create a transfer transaction")
//     }

//     // async fn current_nonce(&self, account: &AccountId) -> Result<Nonce> {
//     //     self.api
//     //         .blocks
//     //         .at(self.finalized_head_number_and_hash().await?.0)
//     //         .await
//     //         .context("failed to obtain the best block for fetching an account nonce")?
//     //         .account_nonce(account)
//     //         .await
//     //         .context("failed to fetch an account nonce by the API client")
//     // }

//     // async fn process_unpaid(
//     //     &self,
//     //     block: &Block<RuntimeConfig, OnlineClient>,
//     //     mut changes: InvoiceChanges,
//     //     hash: BlockHash,
//     //     invoice: AccountId,
//     //     price: Balance,
//     // ) -> Result<()> {
//     //     let balance = self.balance(hash, &invoice).await?;

//     //     if let Some(_remaining) = balance.checked_sub(*price) {
//     //         changes.invoice.status = InvoiceStatus::Paid(price);

//     //         let block_nonce = block
//     //             .account_nonce(&invoice)
//     //             .await
//     //             .context(BLOCK_NONCE_ERROR)?;
//     //         let current_nonce = self.current_nonce(&invoice).await?;

//     //         if current_nonce <= block_nonce {
//     //             let properties = self.database.properties().await;
//     //             let block_hash_count = properties.block_hash_count;
//     //             let signer = changes.invoice.signer(self.database.pair())?;

//     //             let transfers = vec![construct_transfer(
//     //                 &changes.invoice.recipient,
//     //                 price - EXPECTED_USDX_FEE,
//     //                 self.database.properties().await.usd_asset,
//     //             )];
//     //             let tx = self
//     //                 .batch_transfer(current_nonce, block_hash_count, &signer, transfers.clone())
//     //                 .await?;

//     //             self.methods
//     //                 .author_submit_extrinsic(tx.encoded())
//     //                 .await
//     //                 .context("failed to submit an extrinsic")
//     //                 .unwrap();
//     //         }
//     //     }

//     //     Ok(())
//     // }
// }

// fn construct_transfer(to: &AccountId, amount: u128) -> Value {
//     const TRANSFER_KEEP_ALIVE: &str = "transfer";

//     dbg!(amount);

//     dynamic::tx(
//         ASSETS,
//         TRANSFER_KEEP_ALIVE,
//         vec![
//             1337.into(),
//             scale_value::value!(Id(Value::from_bytes(to))),
//             amount.into(),
//         ],
//     )
//     .into_value()
// }

// #[derive(Debug)]
// struct InvoiceChanges {
//     invoice: Invoicee,
//     incoming: HashMap<AccountId, Balance>,
// }

// #[derive(Deserialize, Debug)]
// struct Transferred {
//     asset_id: u32,
//     // The implementation of `Deserialize` for `AccountId32` works only with strings.
//     #[serde(deserialize_with = "account_deserializer")]
//     from: AccountId,
//     #[serde(deserialize_with = "account_deserializer")]
//     to: AccountId,
//     amount: u128,
// }

// fn account_deserializer<'de, D>(deserializer: D) -> Result<AccountId, D::Error>
// where
//     D: Deserializer<'de>,
// {
//     <([u8; 32],)>::deserialize(deserializer).map(|address| AccountId::new(address.0))
// }

// // impl Transferred {
// //     fn process(
// //         self,
// //         invoices_changes: &mut HashMap<AccountId, InvoiceChanges>,
// //         invoices: &mut HashMap<String, Invoicee>,
// //     ) -> Result<()> {
// //         let usd_asset = 1337u32;

// //         tracing::debug!("Transferred event: {self:?}");

// //         if self.from == self.to || self.amount == 0 || self.asset_id != usd_asset {
// //             return Ok(());
// //         }

// //         match invoices_changes.entry(self.to) {
// //             Entry::Occupied(entry) => {
// //                 entry
// //                     .into_mut()
// //                     .incoming
// //                     .entry(self.from)
// //                     .and_modify(|amount| *amount = Balance(amount.saturating_add(self.amount)))
// //                     .or_insert(Balance(self.amount));
// //             }
// //             Entry::Vacant(entry) => {
// //                 if let (None, Some(encoded_invoice)) =
// //                     (invoices.get(&self.from)?, invoices.get(entry.key())?)
// //                 {
// //                     entry.insert(InvoiceChanges {
// //                         invoice: encoded_invoice.value(),
// //                         incoming: [(self.from, self.amount)].into(),
// //                     });
// //                 }
// //             }
// //         }

// //         Ok(())
// //     }
// // }
