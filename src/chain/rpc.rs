//! Blockchain operations that actually require calling the chain

use crate::{
    chain::{
        definitions::{EventFilter, WatchAccount,},
        utils::{asset_balance_query, base58prefix, block_number_query, events_entry_metadata,
        hashed_key_element, pallet_index, storage_key, system_balance_query,
        system_properties_to_short_specs, unit, was_balance_received_at_account},
    },
    definitions::api_v2::{CurrencyProperties, OrderInfo},
    definitions::{
        api_v2::{AssetId, BlockNumber, CurrencyInfo, Decimals, TokenKind},
        AssetInfo, Balance, BlockHash, Chain, NativeToken, Nonce, PalletIndex, Timestamp,
    },
    error::{Error, ErrorChain, NotHex},
    signer::Signer,
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

pub async fn subscribe_blocks(client: &WsClient) -> Result<Subscription<BlockHead>, ErrorChain> {
    Ok(client
            .subscribe(
                "chain_subscribeFinalizedHeads",
                rpc_params![],
                "unsubscribe blocks",
            )
            .await?)
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
pub async fn genesis_hash(client: &WsClient) -> Result<BlockHash, ErrorChain> {
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

/// fetch block hash, to request later the metadata and specs for
/// the same block
pub async fn block_hash(
    client: &WsClient,
    number: Option<String>,
) -> Result<BlockHash, ErrorChain> {
    let rpc_params = if let Some(a) = number {
        rpc_params![a]
    } else {
        rpc_params![]
    };
    let block_hash_request: Value = client
        .request("chain_getBlockHash", rpc_params)
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
pub async fn metadata(client: &WsClient, block: &BlockHash) -> Result<RuntimeMetadataV15, ErrorChain> {
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
pub async fn specs(
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

pub async fn next_block_number(blocks: &mut Subscription<BlockHead>) -> Result<String, ErrorChain> {
    match blocks.next().await {
        Some(Ok(a)) => Ok(a.number),
        Some(Err(e)) => Err(e.into()),
        None => Err(ErrorChain::BlockSubscriptionTerminated),
    }
}

pub async fn next_block(
    client: &WsClient,
    blocks: &mut Subscription<BlockHead>,
) -> Result<H256, ErrorChain> {
    block_hash(&client, Some(next_block_number(blocks).await?)).await
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct BlockHead {
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

pub async fn system_balance_at_account(
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

async fn transfer_events(
    client: &WsClient,
    block: &H256,
    metadata_v15: &RuntimeMetadataV15,
) -> Result<Vec<Event>, ErrorChain> {
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
                } else {
                    return Err(ErrorChain::StorageFormatError);
                };
                let storage_data = decode_as_storage_entry::<&[u8], (), RuntimeMetadataV15>(
                    &key_bytes.as_ref(),
                    &value_bytes.as_ref(),
                    &mut (),
                    events_entry_metadata,
                    types,
                )
                .expect("RAM stored metadata access");
                if let ParsedData::SequenceRaw(sequence_raw) = storage_data.value.data {
                    for sequence_element in sequence_raw.data {
                        if let ParsedData::Composite(event_record) = sequence_element {
                            for event_record_element in event_record {
                                if event_record_element.field_name == Some("event".to_string()) {
                                    if let ParsedData::Event(Event(ref event)) =
                                        event_record_element.data.data
                                    {
                                        if let Some(ref filter) = optional_filter {
                                            if let Some(event_variant) =
                                                filter.optional_event_variant
                                            {
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

pub async fn current_block_number(
    client: &WsClient,
    metadata: &RuntimeMetadataV15,
    block_hash: &H256,
) -> u32 {
    let block_hash = &hex::encode(&block_hash.0);
    let block_number_query = block_number_query(metadata);
    if let Value::String(hex_data) =
        get_value_from_storage(client, &block_number_query.key, block_hash)
            .await
            .unwrap()
    {
        let value_data = hex::decode(hex_data.trim_start_matches("0x")).unwrap();
        let value = decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            &block_number_query.value_ty,
            &value_data.as_ref(),
            &mut (),
            &metadata.types,
        )
        .unwrap();
        if let ParsedData::PrimitiveU32 {
            value,
            specialty: _,
        } = value.data
        {
            value
        } else {
            panic!("unexpected block number format")
        }
    } else {
        panic!("not a string data")
    }
}

pub async fn get_nonce(
    client: &WsClient,
    account_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let rpc_params = rpc_params![account_id];
    println!("{rpc_params:?}");
    let nonce: Value = client.request("account_nextIndex", rpc_params).await?;
    println!("{nonce:?}");
    Ok(())
}

pub async fn send_stuff(client: &WsClient, data: &str) -> Result<(), ErrorChain> {
    let rpc_params = rpc_params![data];
    let mut subscription: Subscription<Value> = client
        .subscribe("author_submitAndWatchExtrinsic", rpc_params, "")
        .await?;
    let reply = subscription.next().await.unwrap();
    //println!("{reply:?}"); // TODO!
    Ok(())
}
