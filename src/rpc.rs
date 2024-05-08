use crate::{
    chain::{
        asset_balance_query, base58prefix, events_entry_metadata, hashed_key_element, pallet_index, storage_key,
        system_balance_query, system_properties_to_short_specs, unit,
    },
    definitions::api_v2::{CurrencyProperties, OrderInfo},
    definitions::{
        api_v2::{AssetId, BlockNumber, CurrencyInfo, Decimals, TokenKind},
        AssetInfo, Balance, BlockHash, Chain, NativeToken, Nonce, PalletIndex, Timestamp,
    },
    error::{Error, ErrorChain, NotHex},
    state::State,
    utils::unhex,
    TaskTracker,
};
use frame_metadata::{
    v15::{RuntimeMetadataV15, StorageEntryMetadata, StorageEntryType},
    RuntimeMetadata,
};
use jsonrpsee::core::client::{ClientT, Subscription, SubscriptionClientT};
use jsonrpsee::rpc_params;
use jsonrpsee::ws_client::{WsClient, WsClientBuilder};
use parity_scale_codec::{Decode, DecodeAll, Encode};
use primitive_types::H256;
use scale_info::{form::PortableForm, PortableRegistry, TypeDef, TypeDefPrimitive};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};
use sp_crypto_hashing::{blake2_128, blake2_256, twox_128, twox_256, twox_64};
use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap},
    fmt::Debug,
    num::NonZeroU64,
};
use substrate_crypto_light::common::{AccountId32, AsBase58};
use substrate_parser::{
    cards::{Event, ExtendedData, FieldData, ParsedData, Sequence},
    decode_all_as_type, decode_as_storage_entry,
    special_indicators::SpecialtyUnsignedInteger,
    storage_data::{KeyData, KeyPart},
    AsMetadata, ShortSpecs,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::{timeout, Duration},
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
const TRANSFER: &str = "Transfer";

// Runtime APIs

const AURA: &str = "AuraApi";

#[derive(Debug)]
pub struct EventFilter<'a> {
    pub pallet: &'a str,
    pub optional_event_variant: Option<&'a str>,
}

#[derive(Debug)]
struct ChainProperties {
    specs: ShortSpecs,
    metadata: RuntimeMetadataV15,
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

pub async fn get_value_from_storage(
    client: &WsClient,
    whole_key: &str,
    block_hash: &str,
) -> Result<Value, ErrorChain> {
    let value: Value = client
        .request("state_getStorage", rpc_params![whole_key, block_hash])
        .await?;
    Ok(value)
}

pub async fn get_keys_from_storage(
    client: &WsClient,
    prefix: &str,
    storage_name: &str,
    block_hash: &str,
) -> Result<Value, ErrorChain> {
    let keys: Value = client
        .request(
            "state_getKeys",
            rpc_params![
                format!(
                    "0x{}{}",
                    hex::encode(twox_128(prefix.as_bytes())),
                    hex::encode(twox_128(storage_name.as_bytes()))
                ),
                block_hash
            ],
        )
        .await?;
    Ok(keys)
}

/// fetch genesis hash, must be a hexadecimal string transformable into
/// H256 format
async fn genesis_hash(client: &WsClient) -> Result<BlockHash, ErrorChain> {
    let genesis_hash_request: Value = client
        .request(
            "chain_getBlockHash",
            rpc_params![Value::Number(Number::from(0u8))],
        )
        .await
        .map_err(ErrorChain::Client)?;
    match genesis_hash_request {
        Value::String(x) => {
            let genesis_hash_raw = unhex(&x, NotHex::GenesisHash)?;
            Ok(H256(
                genesis_hash_raw
                    .try_into()
                    .map_err(|_| ErrorChain::GenesisHashLength)?,
            ))
        }
        _ => return Err(ErrorChain::GenesisHashFormat),
    }
}

/// fetch current block hash, to request later the metadata and specs for
/// the same block
async fn block_hash(client: &WsClient, number: String) -> Result<BlockHash, ErrorChain> {
    let block_hash_request: Value = client
        .request("chain_getBlockHash", rpc_params![number])
        .await
        .map_err(ErrorChain::Client)?;
    match block_hash_request {
        Value::String(x) => {
            let block_hash_raw = unhex(&x, NotHex::BlockHash)?;
            Ok(H256(
                block_hash_raw
                    .try_into()
                    .map_err(|_| ErrorChain::BlockHashLength)?,
            ))
        }
        _ => return Err(ErrorChain::BlockHashFormat),
    }
}

/// fetch metadata at known block
async fn metadata(client: &WsClient, block: &BlockHash) -> Result<RuntimeMetadataV15, ErrorChain> {
    let block_hash_string = block.to_string();
    let metadata_request: Value = client
        .request(
            "state_call",
            rpc_params![
                "Metadata_metadata_at_version",
                "0f000000",
                &block_hash_string
            ],
        )
        .await
        .map_err(ErrorChain::Client)?;
    match metadata_request {
        Value::String(x) => {
            let metadata_request_raw = unhex(&x, NotHex::Metadata)?;
            let maybe_metadata_raw = Option::<Vec<u8>>::decode_all(&mut &metadata_request_raw[..])
                .map_err(|_| ErrorChain::RawMetadataNotDecodeable)?;
            if let Some(meta_v15_bytes) = maybe_metadata_raw {
                if meta_v15_bytes.starts_with(b"meta") {
                    match RuntimeMetadata::decode_all(&mut &meta_v15_bytes[4..]) {
                        Ok(RuntimeMetadata::V15(runtime_metadata_v15)) => {
                            return Ok(runtime_metadata_v15)
                        }
                        Ok(_) => return Err(ErrorChain::NoMetadataV15),
                        Err(_) => return Err(ErrorChain::MetadataNotDecodeable),
                    }
                } else {
                    return Err(ErrorChain::NoMetaPrefix);
                }
            } else {
                return Err(ErrorChain::NoMetadataV15);
            }
        }
        _ => return Err(ErrorChain::MetadataFormat),
    };
}

// fetch specs at known block
async fn specs(
    client: &WsClient,
    metadata: &RuntimeMetadataV15,
    block_hash: &BlockHash,
) -> Result<ShortSpecs, ErrorChain> {
    let specs_request: Value = client
        .request("system_properties", rpc_params![hex::encode(&block_hash.0)])
        .await?;
    //.map_err(ErrorChain::Client)?;
    match specs_request {
        Value::Object(properties) => system_properties_to_short_specs(&properties, &metadata),
        _ => return Err(ErrorChain::PropertiesFormat),
    }
}

#[derive(Debug)]
pub struct Currency {
    chain: String,
    asset: Option<AssetId>,
}

#[derive(Debug)]
pub struct ConnectedChain {
    rpc: String,
    client: WsClient,
    genesis: BlockHash,
    properties: ChainProperties,
}

/// RPC server handle
#[derive(Clone, Debug)]
pub struct ChainManager {
    pub tx: tokio::sync::mpsc::Sender<ChainRequest>,
}

impl ChainManager {
    /// Run once to start all chain connections; this should be very robust, if manager fails
    /// - all modules should be restarted probably.
    pub fn ignite(
        chain: Vec<Chain>,
        state: State,
        task_tracker: TaskTracker,
        cancellation_token: CancellationToken,
    ) -> Result<Self, Error> {
        /*
                let client = WsClientBuilder::default()
                    .build(rpc.clone())
                    .await
                    .map_err(ErrorChain::Client)?;
        */
        let (tx, mut rx) = mpsc::channel(1024);

        let mut watch_chain = HashMap::new();

        let mut currency_map = HashMap::new();

        // start network monitors
        for c in chain {
            let (chain_tx, mut chain_rx) = mpsc::channel(1024);
            watch_chain.insert(c.name.clone(), chain_tx.clone());
            if let Some(ref a) = c.native_token {
                if let Some(_) = currency_map.insert(a.name.clone(), c.name.clone()) {
                    return Err(Error::DuplicateCurrency(a.name.clone()));
                }
            }
            for a in &c.asset {
                if let Some(_) = currency_map.insert(a.name.clone(), c.name.clone()) {
                    return Err(Error::DuplicateCurrency(a.name.clone()));
                }
            }

            start_chain_watch(
                c,
                &currency_map,
                chain_tx.clone(),
                chain_rx,
                state.interface(),
                task_tracker.clone(),
                cancellation_token.clone(),
            )?;
        }

        task_tracker
            .clone()
            .spawn("Blockchain connections manager", async move {
                // start requests engine
                while let Some(request) = rx.recv().await {
                    match request {
                        ChainRequest::WatchAccount(request) => {
                            if let Some(chain) = currency_map.get(&request.currency) {
                                if let Some(receiver) = watch_chain.get(chain) {
                                    let _unused = receiver
                                        .send(ChainTrackerRequest::WatchAccount(request))
                                        .await;
                                } else {
                                    let _unused = request
                                        .res
                                        .send(Err(ErrorChain::InvalidChain(chain.to_string())));
                                }
                            } else {
                                let _unused = request
                                    .res
                                    .send(Err(ErrorChain::InvalidCurrency(request.currency)));
                            }
                        }
                        ChainRequest::Reap(request) => {todo!()}
                        ChainRequest::Shutdown(res) => {
                            for (name, chain) in watch_chain.drain() {
                                let (tx, rx) = oneshot::channel();
                                let _unused = chain.send(ChainTrackerRequest::Shutdown(tx)).await;
                                let _ = rx.await;
                            }
                            let _ = res.send(());
                            break;
                        }
                    }
                }

                Ok("Chain manager is shutting down".into())
            });

        Ok(Self { tx })
    }

    pub async fn add_invoice(&self, id: String, order: OrderInfo) -> Result<(), ErrorChain> {
        let (res, rx) = oneshot::channel();
        self.tx
            .send(ChainRequest::WatchAccount(WatchAccount::new(id, order, res)?))
            .await
            .map_err(|_| ErrorChain::MessageDropped)?;
        rx.await.map_err(|_| ErrorChain::MessageDropped)?
    }

    pub async fn reap(&self, id: String, order: OrderInfo) -> Result<(), ErrorChain> {
         let (res, rx) = oneshot::channel();
        self.tx
            .send(ChainRequest::Reap(WatchAccount::new(id, order, res)?))
            .await
            .map_err(|_| ErrorChain::MessageDropped)?;
        rx.await.map_err(|_| ErrorChain::MessageDropped)?

    }

    pub async fn shutdown(&self) -> () {
        let (tx, rx) = oneshot::channel();
        let _unused = self.tx.send(ChainRequest::Shutdown(tx)).await;
        let _ = rx.await;
        ()
    }
}

enum ChainRequest {
    WatchAccount(WatchAccount),
    Shutdown(oneshot::Sender<()>),
    Reap(WatchAccount),
}

enum ChainTrackerRequest {
    WatchAccount(WatchAccount),
    Shutdown(oneshot::Sender<()>),
    NewBlock(String),
}

#[derive(Debug)]
struct WatchAccount {
    id: String,
    address: AccountId32,
    currency: String,
    amount: Balance,
    res: oneshot::Sender<Result<(), ErrorChain>>,
}

impl WatchAccount {
    fn new(
        id: String,
        order: OrderInfo,
        res: oneshot::Sender<Result<(), ErrorChain>>,
    ) -> Result<WatchAccount, ErrorChain> {
        Ok(WatchAccount {
            id: id,
            address: AccountId32::from_base58_string(&order.payment_account)
                .map_err(ErrorChain::InvoiceAccount)?
                .0,
            currency: order.currency.currency,
            amount: Balance::parse(order.amount, order.currency.decimals),
            res,
        })
    }
}

#[derive(Clone, Debug)]
struct Invoice {
    id: String,
    address: AccountId32,
    currency: String,
    amount: Balance,
}

impl Invoice {
    fn from_request(watch_account: WatchAccount) -> Self {
        drop(watch_account.res.send(Ok(())));
        Invoice {
            id: watch_account.id,
            address: watch_account.address,
            currency: watch_account.currency,
            amount: watch_account.amount,
        }
    }

    async fn check(
        &self,
        client: &WsClient,
        metadata: &RuntimeMetadataV15,
        block: &H256,
        currency: &HashMap<String, CurrencyProperties>,
    ) -> Result<bool, ErrorChain> {
        let currency = currency
            .get(&self.currency)
            .ok_or(ErrorChain::InvalidCurrency(self.currency.clone()))?;
        if let Some(asset_id) = currency.asset_id {
            let balance =
                asset_balance_at_account(client, &block, &metadata, &self.address, asset_id)
                    .await?;
            Ok(balance >= self.amount)
        } else {
            let balance =
                system_balance_at_account(client, &block, &metadata, &self.address).await?;
            Ok(balance >= self.amount)
        }
    }
}

fn start_chain_watch(
    c: Chain,
    currency_map: &HashMap<String, String>,
    chain_tx: mpsc::Sender<ChainTrackerRequest>,
    mut chain_rx: mpsc::Receiver<ChainTrackerRequest>,
    state: State,
    task_tracker: TaskTracker,
    cancellation_token: CancellationToken,
) -> Result<(), ErrorChain> {
    //let (block_source_tx, mut block_source_rx) = mpsc::channel(16);
    task_tracker
        .clone()
        .spawn(format!("Chain {} watcher", c.name.clone()), async move {
            let watchdog = 30000;
            let mut watched_accounts = Vec::new();
            let mut shutdown = false;
            for endpoint in c.endpoints.iter().cycle() {
                // not restarting chain if shutdown is in progress
                if shutdown || cancellation_token.is_cancelled() {
                    break;
                }
                if let Ok(client) = WsClientBuilder::default().build(endpoint).await {
                    // prepare chain
                    match prepare_chain(&client, &mut watched_accounts, endpoint, chain_tx.clone(), state.interface(), task_tracker.clone())
                        .await
                    {
                        Ok(_) => (),
                        Err(e) => {
                            tracing::info!(
                                "Failed to connect to chain {}, due to {:?} switching RPC server...",
                                c.name,
                                e
                            );
                            continue;
                        }
                    }


                    // fulfill requests
                    while let Ok(Some(request)) =
                        timeout(Duration::from_millis(watchdog), chain_rx.recv()).await
                    {
                        match request {
                            ChainTrackerRequest::NewBlock(block) => {
                                let block = block_hash(&client, block);
                                
                            }
                            ChainTrackerRequest::WatchAccount(request) => {
                                watched_accounts.push(Invoice::from_request(request));
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

async fn prepare_chain(
    client: &WsClient,
    watched_accounts: &mut Vec<Invoice>,
    rpc_url: &str,
    chain_tx: mpsc::Sender<ChainTrackerRequest>,
    state: State,
    task_tracker: TaskTracker,
) -> Result<(), ErrorChain> {
    let genesis_hash = genesis_hash(&client).await?;
    let mut blocks: Subscription<BlockHead> = client
        .subscribe(
            "chain_subscribeFinalizedHeads",
            rpc_params![],
            "unsubscribe blocks",
        )
        .await?;
    let block = next_block(client, &mut blocks).await?;
    let metadata = metadata(&client, &block).await?;
    let specs = specs(&client, &metadata, &block).await?;
    let assets = assets_set_at_block(&client, &block, &metadata, rpc_url, specs).await?;

    // check monitored accounts
    let mut new_accounts = Vec::new();
    for invoice in watched_accounts.iter() {
        match invoice.check(client, &metadata, &block, &assets).await {
            Ok(true) => {
                state.order_paid(invoice.id.clone());
            }
            Ok(false) => new_accounts.push(invoice.to_owned()),
            Err(e) => {
                tracing::warn!("account fetch error: {0:?}", e);
                new_accounts.push(invoice.to_owned());
            }
        }
    }
    *watched_accounts = new_accounts;

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

    Ok(())
}

async fn next_block_number(blocks: &mut Subscription<BlockHead>) -> Result<String, ErrorChain> {
    match blocks.next().await {
        Some(Ok(a)) => Ok(a.number),
        Some(Err(e)) => Err(e.into()),
        None => Err(ErrorChain::BlockSubscriptionTerminated),
    }
}

async fn next_block(
    client: &WsClient,
    blocks: &mut Subscription<BlockHead>,
) -> Result<H256, ErrorChain> {
    block_hash(&client, next_block_number(blocks).await?).await
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
struct BlockHead {
    //digest: Value,
    //extrinsics_root: String,
    pub number: String,
    //parent_hash: String,
    //state_root: String,
}
 /*
#[derive(Deserialize, Debug)]
struct Transferred {
    asset_id: u32,
    // The implementation of `Deserialize` for `AccountId32` works only with strings.
    #[serde(deserialize_with = "account_deserializer")]
    from: AccountId32,
    #[serde(deserialize_with = "account_deserializer")]
    to: AccountId32,
    amount: u128,
}*/
/*
fn account_deserializer<'de, D>(deserializer: D) -> Result<AccountId32, D::Error>
where
    D: Deserializer<'de>,
{
    <([u8; 32],)>::deserialize(deserializer).map(|address| AccountId32(address.0))
}*/

// TODO: add proper errors
//
// not urgent since it happens on boot once for now
//
/// Get all sufficient assets from a chain
pub async fn assets_set_at_block(
    client: &WsClient,
    block_hash: &H256,
    metadata_v15: &RuntimeMetadataV15,
    rpc_url: &str,
    specs: ShortSpecs,
) -> Result<HashMap<String, CurrencyProperties>, ErrorChain> {
    let block_hash = &hex::encode(&block_hash.0);
    let mut assets_set = HashMap::new();
    let chain_name =
        <RuntimeMetadataV15 as AsMetadata<()>>::spec_name_version(metadata_v15)?.spec_name;
    assets_set.insert(
        specs.unit,
        CurrencyProperties {
            chain_name: chain_name.clone(),
            kind: TokenKind::Balances,
            decimals: specs.decimals,
            rpc_url: rpc_url.to_owned(),
            asset_id: None,
            ss58: specs.base58prefix,
        },
    );

    let mut assets_asset_storage_metadata = None;
    let mut assets_metadata_storage_metadata = None;

    for pallet in metadata_v15.pallets.iter() {
        if let Some(storage) = &pallet.storage {
            if storage.prefix == "Assets" {
                for entry in storage.entries.iter() {
                    if entry.name == "Asset" {
                        assets_asset_storage_metadata = Some(entry);
                    }
                    if entry.name == "Metadata" {
                        assets_metadata_storage_metadata = Some(entry);
                    }
                    if assets_asset_storage_metadata.is_some()
                        && assets_metadata_storage_metadata.is_some()
                    {
                        break;
                    }
                }
                break;
            }
        }
    }

    let assets_asset_storage_metadata = assets_asset_storage_metadata.unwrap();
    let assets_metadata_storage_metadata = assets_metadata_storage_metadata.unwrap();

    let available_keys_assets_asset =
        get_keys_from_storage(client, "Assets", "Asset", block_hash).await?;
    if let Value::Array(ref keys_array) = available_keys_assets_asset {
        for key in keys_array.iter() {
            if let Value::String(string_key) = key {
                let value_fetch = get_value_from_storage(client, string_key, block_hash)
                    .await
                    .unwrap();
                if let Value::String(ref string_value) = value_fetch {
                    let key_data = hex::decode(string_key.trim_start_matches("0x")).unwrap();
                    let value_data = hex::decode(string_value.trim_start_matches("0x")).unwrap();
                    let storage_entry = decode_as_storage_entry::<&[u8], (), RuntimeMetadataV15>(
                        &key_data.as_ref(),
                        &value_data.as_ref(),
                        &mut (),
                        assets_asset_storage_metadata,
                        &metadata_v15.types,
                    )
                    .unwrap();
                    let asset_id = {
                        if let KeyData::SingleHash { content } = storage_entry.key {
                            if let KeyPart::Parsed(extended_data) = content {
                                if let ParsedData::PrimitiveU32 {
                                    value,
                                    specialty: _,
                                } = extended_data.data
                                {
                                    //println!("got asset id: {value}");
                                    value
                                } else {
                                    panic!("asset id not u32")
                                }
                            } else {
                                panic!("assets asset key has no parseable part")
                            }
                        } else {
                            panic!("assets asset key is not a single hash entity")
                        }
                    };
                    let mut verified_sufficient = false;
                    if let ParsedData::Composite(fields) = storage_entry.value.data {
                        for field_data in fields.iter() {
                            if let Some(field_name) = &field_data.field_name {
                                if field_name == "is_sufficient" {
                                    if let ParsedData::PrimitiveBool(is_it) = field_data.data.data {
                                        verified_sufficient = is_it;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    if verified_sufficient {
                        match &assets_metadata_storage_metadata.ty {
                            StorageEntryType::Plain(_) => {
                                panic!("expected map with single entry, got plain")
                            }
                            StorageEntryType::Map {
                                hashers,
                                key: key_ty,
                                value: value_ty,
                            } => {
                                if hashers.len() == 1 {
                                    let hasher = &hashers[0];
                                    match metadata_v15.types.resolve(key_ty.id).unwrap().type_def {
                                        TypeDef::Primitive(TypeDefPrimitive::U32) => {
                                            let key_assets_metadata = format!(
                                                "0x{}{}{}",
                                                hex::encode(twox_128("Assets".as_bytes())),
                                                hex::encode(twox_128("Metadata".as_bytes())),
                                                hex::encode(hashed_key_element(
                                                    &asset_id.encode(),
                                                    hasher
                                                ))
                                            );
                                            let value_fetch = get_value_from_storage(
                                                client,
                                                &key_assets_metadata,
                                                block_hash,
                                            )
                                            .await
                                            .unwrap();
                                            if let Value::String(ref string_value) = value_fetch {
                                                let value_data = hex::decode(
                                                    string_value.trim_start_matches("0x"),
                                                )
                                                .unwrap();
                                                let value = decode_all_as_type::<
                                                    &[u8],
                                                    (),
                                                    RuntimeMetadataV15,
                                                >(
                                                    value_ty,
                                                    &value_data.as_ref(),
                                                    &mut (),
                                                    &metadata_v15.types,
                                                )
                                                .unwrap();

                                                let mut name = None;
                                                let mut symbol = None;
                                                let mut decimals = None;

                                                if let ParsedData::Composite(fields) = value.data {
                                                    for field_data in fields.iter() {
                                                        if let Some(field_name) =
                                                            &field_data.field_name
                                                        {
                                                            match field_name.as_str() {
                                                                "name" => match &field_data.data.data {
                                                                    ParsedData::Text{text, specialty: _} => {
                                                                        name = Some(text.to_owned());
                                                                    },
                                                                    ParsedData::Sequence(sequence) => {
                                                                        if let Sequence::U8(bytes) = &sequence.data {
                                                                            if let Ok(name_from_bytes) = String::from_utf8(bytes.to_owned()) {
                                                                                name = Some(name_from_bytes);
                                                                            }
                                                                        }
                                                                    }
                                                                    ParsedData::Composite(fields) => {
                                                                        if fields.len() == 1 {
                                                                            match &fields[0].data.data {
                                                                                ParsedData::Text{text, specialty: _} => {
                                                                                    name = Some(text.to_owned());
                                                                                },
                                                                                ParsedData::Sequence(sequence) => {
                                                                                    if let Sequence::U8(bytes) = &sequence.data {
                                                                                        if let Ok(name_from_bytes) = String::from_utf8(bytes.to_owned()) {
                                                                                            name = Some(name_from_bytes);
                                                                                        }
                                                                                    }
                                                                                },
                                                                                _ => {},
                                                                            }
                                                                        }
                                                                    },
                                                                    _ => {},
                                                                },
                                                                "symbol" => match &field_data.data.data {
                                                                    ParsedData::Text{text, specialty: _} => {
                                                                        symbol = Some(text.to_owned());
                                                                    },
                                                                    ParsedData::Sequence(sequence) => {
                                                                        if let Sequence::U8(bytes) = &sequence.data {
                                                                            if let Ok(symbol_from_bytes) = String::from_utf8(bytes.to_owned()) {
                                                                                symbol = Some(symbol_from_bytes);
                                                                            }
                                                                        }
                                                                    }
                                                                    ParsedData::Composite(fields) => {
                                                                        if fields.len() == 1 {
                                                                            match &fields[0].data.data {
                                                                                ParsedData::Text{text, specialty: _} => {
                                                                                    symbol = Some(text.to_owned());
                                                                                },
                                                                                ParsedData::Sequence(sequence) => {
                                                                                    if let Sequence::U8(bytes) = &sequence.data {
                                                                                        if let Ok(symbol_from_bytes) = String::from_utf8(bytes.to_owned()) {
                                                                                            symbol = Some(symbol_from_bytes);
                                                                                        }
                                                                                    }
                                                                                },
                                                                                _ => {},
                                                                            }
                                                                        }
                                                                    },
                                                                    _ => {},
                                                                },
                                                                "decimals" => {
                                                                    if let ParsedData::PrimitiveU8{value, specialty: _} = field_data.data.data {
                                                                        decimals = Some(value);
                                                                    }
                                                                },
                                                                _ => {},
                                                            }
                                                        }
                                                        if name.is_some()
                                                            && symbol.is_some()
                                                            && decimals.is_some()
                                                        {
                                                            break;
                                                        }
                                                    }
                                                    //let name = name.unwrap();
                                                    let symbol = symbol.unwrap();
                                                    let decimals = decimals.unwrap();
                                                    assets_set.insert(
                                                        symbol,
                                                        CurrencyProperties {
                                                            chain_name: chain_name.clone(),
                                                            kind: TokenKind::Asset,
                                                            decimals,
                                                            rpc_url: rpc_url.to_string(),
                                                            asset_id: Some(asset_id),
                                                            ss58: specs.base58prefix,
                                                        },
                                                    );
                                                } else {
                                                    panic!("unexpected assets metadata value structure")
                                                }
                                            }
                                        }
                                        _ => panic!("wrong data type"),
                                    }
                                } else {
                                    panic!("expected map with single entry, got multiple entries")
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(assets_set)
}

pub async fn asset_balance_at_account(
    client: &WsClient,
    block_hash: &H256,
    metadata_v15: &RuntimeMetadataV15,
    account_id: &AccountId32,
    asset_id: AssetId,
) -> Result<Balance, ErrorChain> {
    let block_hash = &hex::encode(&block_hash.0);
    let query = asset_balance_query(metadata_v15, account_id, asset_id)?;

    let value_fetch = get_value_from_storage(client, &query.key, block_hash)
        .await
        .unwrap();
    if let Value::String(ref string_value) = value_fetch {
        let value_data = hex::decode(string_value.trim_start_matches("0x")).unwrap();
        let value = decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            &query.value_ty,
            &value_data.as_ref(),
            &mut (),
            &metadata_v15.types,
        )
        .unwrap();
        if let ParsedData::Composite(fields) = value.data {
            for field in fields.iter() {
                if let ParsedData::PrimitiveU128 {
                    value,
                    specialty: SpecialtyUnsignedInteger::Balance,
                } = field.data.data
                {
                    return Ok(Balance(value));
                }
            }
            panic!();
        } else {
            panic!()
        }
    } else {
        panic!()
    }
}

async fn system_balance_at_account(
    client: &WsClient,
    block_hash: &H256,
    metadata_v15: &RuntimeMetadataV15,
    account_id: &AccountId32,
) -> Result<Balance, ErrorChain> {
    let block_hash = &hex::encode(&block_hash.0);
    let query = system_balance_query(metadata_v15, account_id)?;

    let value_fetch = get_value_from_storage(client, &query.key, block_hash)
        .await
        .unwrap();
    if let Value::String(ref string_value) = value_fetch {
        let value_data = hex::decode(string_value.trim_start_matches("0x")).unwrap();
        let value = decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            &query.value_ty,
            &value_data.as_ref(),
            &mut (),
            &metadata_v15.types,
        )
        .unwrap();
        if let ParsedData::Composite(fields) = value.data {
            for field in fields.iter() {
                if field.field_name == Some("data".to_string()) {
                    if let ParsedData::Composite(inner_fields) = &field.data.data {
                        for inner_field in inner_fields.iter() {
                            if inner_field.field_name == Some("free".to_string()) {
                                if let ParsedData::PrimitiveU128 {
                                    value,
                                    specialty: SpecialtyUnsignedInteger::Balance,
                                } = inner_field.data.data
                                {
                                    return Ok(Balance(value));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Err(ErrorChain::BalanceNotFound)
}

async fn transfer_events(client: &WsClient, block: &H256, metadata_v15: &RuntimeMetadataV15) -> Result<Vec<Event>, ErrorChain> {
    let events_entry_metadata = events_entry_metadata(&metadata_v15)?;

    events_at_block(
        &client,
        block,
        Some(EventFilter {
            pallet: BALANCES,
            optional_event_variant: Some(TRANSFER),
        }),
        events_entry_metadata,
        &metadata_v15.types,
    )
    .await
}

pub async fn events_at_block(
    client: &WsClient,
    block_hash: &H256,
    optional_filter: Option<EventFilter<'_>>,
    events_entry_metadata: &StorageEntryMetadata<PortableForm>,
    types: &PortableRegistry,
) -> Result<Vec<Event>, ErrorChain> {
    let block_hash = &hex::encode(&block_hash.0);
    let keys_from_storage = get_keys_from_storage(client, "System", "Events", block_hash).await?;
    let mut out = Vec::new();
    if let Value::Array(ref keys_array) = keys_from_storage {
    for key in keys_array {
        if let Value::String(key) = key {
        let data_from_storage = get_value_from_storage(client, &key, block_hash).await?;
        let key_bytes = unhex(&key, NotHex::StorageValue)?;
        let value_bytes = if let Value::String(data_from_storage) = data_from_storage {
        unhex(&data_from_storage, NotHex::StorageValue)?
        } else { return Err(ErrorChain::StorageFormatError) };
        let storage_data = decode_as_storage_entry::<&[u8], (), RuntimeMetadataV15>(
            &key_bytes.as_ref(),
            &value_bytes.as_ref(),
            &mut (),
            events_entry_metadata,
            types,
        ).expect("RAM stored metadata access");
        if let ParsedData::SequenceRaw(sequence_raw) = storage_data.value.data {
            for sequence_element in sequence_raw.data {
                if let ParsedData::Composite(event_record) = sequence_element {
                    for event_record_element in event_record {
                        if event_record_element.field_name == Some("event".to_string()) {
                            if let ParsedData::Event(Event(ref event)) =
                                event_record_element.data.data
                            {
                                if let Some(ref filter) = optional_filter {
                                    if let Some(event_variant) = filter.optional_event_variant {
                                        if event.pallet_name == filter.pallet
                                            && event.variant_name == event_variant
                                        {
                                            out.push(Event(event.to_owned()));
                                        }
                                    } else if event.pallet_name == filter.pallet {
                                        out.push(Event(event.to_owned()));
                                    }
                                } else {
                                    out.push(Event(event.to_owned()));
                                }
                            }
                        }
                    }
                }
            }
        }
    return Ok(out);
        }
    }
    }
    Err(ErrorChain::EventsMissing)
}

