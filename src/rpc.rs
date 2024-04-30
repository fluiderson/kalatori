use crate::{
    chain::{
        base58prefix, hashed_key_element, pallet_index, storage_key,
        system_properties_to_short_specs, unit,
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
    v15::{RuntimeMetadataV15, StorageEntryType},
    RuntimeMetadata,
};
use jsonrpsee::core::client::ClientT;
use jsonrpsee::rpc_params;
use jsonrpsee::ws_client::{WsClient, WsClientBuilder};
use parity_scale_codec::{Decode, DecodeAll, Encode};
use primitive_types::H256;
use scale_info::{TypeDef, TypeDefPrimitive};
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
    cards::{ExtendedData, ParsedData, Sequence},
    decode_all_as_type, decode_as_storage_entry,
    storage_data::{KeyData, KeyPart},
    AsMetadata, ShortSpecs,
};
use tokio::sync::{mpsc, oneshot};
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
) -> Result<Value, Box<dyn std::error::Error>> {
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
) -> Result<Value, Box<dyn std::error::Error>> {
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
async fn block_hash(client: &WsClient) -> Result<BlockHash, ErrorChain> {
    let block_hash_request: Value = client
        .request("chain_getBlockHash", rpc_params![])
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
            watch_chain.insert(c.name.clone(), chain_tx);
            if let Some(a) = c.native_token {
                if let Some(_) = currency_map.insert(a.name.clone(), c.name.clone()) {
                    return Err(Error::DuplicateCurrency(a.name));
                }
            }
            for a in c.asset {
                if let Some(_) = currency_map.insert(a.name.clone(), c.name.clone()) {
                    return Err(Error::DuplicateCurrency(a.name));
                }
            }

            task_tracker.spawn(format!("Chain {} watcher", c.name.clone()), async move {
                let mut watched_accounts = Vec::new();
                let mut shutdown = false;
                for endpoint in c.endpoints.iter().cycle() {
                    // not restarting chain if shutdown is in progress
                    if shutdown {
                        break;
                    }
                    if let Ok(client) = WsClientBuilder::default().build(endpoint).await {
                        while let Some(request) = chain_rx.recv().await {
                            match request {
                                ChainTrackerRequest::WatchAccount(request) => {
                                    watched_accounts.push(request);
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
                                    let _unused = receiver.send(ChainTrackerRequest::WatchAccount(request)).await;
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

    pub async fn add_invoice(&self, order: OrderInfo) -> Result<(), ErrorChain> {
        let (res, rx) = oneshot::channel();
        self.tx
            .send(ChainRequest::WatchAccount(WatchAccount::new(order, res)?)).await.map_err(|_| ErrorChain::MessageDropped)?;
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
}

enum ChainTrackerRequest {
    WatchAccount(WatchAccount),
    Shutdown(oneshot::Sender<()>),
}

struct WatchAccount {
    address: AccountId32,
    currency: String,
    amount: Balance,
    res: oneshot::Sender<Result<(), ErrorChain>>,
}

impl WatchAccount {
    fn new(
        order: OrderInfo,
        res: oneshot::Sender<Result<(), ErrorChain>>,
    ) -> Result<WatchAccount, ErrorChain> {
        Ok(WatchAccount {
            address: AccountId32::from_base58_string(&order.payment_account)
                .map_err(ErrorChain::InvoiceAccount)?
                .0,
            currency: order.currency.currency,
            amount: Balance::parse(order.amount, order.currency.decimals),
            res,
        })
    }
}

#[derive(Deserialize, Debug)]
struct Transferred {
    asset_id: u32,
    // The implementation of `Deserialize` for `AccountId32` works only with strings.
    #[serde(deserialize_with = "account_deserializer")]
    from: AccountId32,
    #[serde(deserialize_with = "account_deserializer")]
    to: AccountId32,
    amount: u128,
}

fn account_deserializer<'de, D>(deserializer: D) -> Result<AccountId32, D::Error>
where
    D: Deserializer<'de>,
{
    <([u8; 32],)>::deserialize(deserializer).map(|address| AccountId32(address.0))
}

// TODO: add proper errors
//
// not urgent since it happens on boot once for now
//
/// Get all sufficient assets from a chain
pub async fn assets_set_at_block(
    client: &WsClient,
    block_hash: &str,
    metadata_v15: RuntimeMetadataV15,
    rpc_url: String,
    ss58: u16,
) -> HashMap<String, CurrencyProperties> {
    //let metadata_v15 = metadata_v15(address, block_hash).await.unwrap();

    let mut assets_set = HashMap::new();

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

    let available_keys_assets_asset = get_keys_from_storage(client, "Assets", "Asset", block_hash)
        .await
        .unwrap();
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
                                                            chain_name: "TODO".into(),
                                                            kind: TokenKind::Asset,
                                                            decimals,
                                                            rpc_url: rpc_url.clone(),
                                                            asset_id: Some(asset_id),
                                                            ss58,
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
    assets_set
}
