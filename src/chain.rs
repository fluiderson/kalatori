//! Utils to process chain data without accessing the chain

//TransactionToFill::init(&mut (), metadata, genesis_hash).unwrap();
use crate::error::ErrorChain;
use frame_metadata::{
    v14::StorageHasher,
    v15::{RuntimeMetadataV15, StorageEntryType},
};
use parity_scale_codec::{Decode, Encode};
use scale_info::{TypeDef, TypeDefPrimitive};
use serde_json::{Map, Number, Value};
use sp_crypto_hashing::{blake2_128, blake2_256, twox_128, twox_256, twox_64};
use substrate_crypto_light::common::{AccountId32, DeriveJunction, FullDerivation};
use substrate_parser::{
    cards::{ExtendedData, ParsedData},
    decode_all_as_type, AsMetadata, ShortSpecs,
};
use substrate_constructor::{fill_prepare::{PrimitiveToFill, SpecialTypeToFill, TypeContentToFill, UnsignedToFill}, storage_query::{EntrySelector, EntrySelectorFunctional, FinalizedStorageQuery, StorageEntryTypeToFill, StorageSelector, StorageSelectorFunctional}};

#[derive(Clone, Debug)]
pub struct FinalizedQueries {
    pub system_account: FinalizedStorageQuery,
    pub assets_account: FinalizedStorageQuery,
}

pub fn balance_queries(
    metadata_v15: &RuntimeMetadataV15,
    account_id: &AccountId32,
    asset_id: u32,
) -> FinalizedQueries {
    let storage_selector = StorageSelector::init(&mut (), metadata_v15).unwrap();

    if let StorageSelector::Functional(mut storage_selector_functional) = storage_selector {
        let mut index_system_in_pallet_selector = None;
        let mut index_assets_in_pallet_selector = None;

        for (index, pallet) in storage_selector_functional
            .available_pallets
            .iter()
            .enumerate()
        {
            match pallet.prefix.as_str() {
                "System" => index_system_in_pallet_selector = Some(index),
                "Assets" => index_assets_in_pallet_selector = Some(index),
                _ => {}
            }
            if index_system_in_pallet_selector.is_some()
                && index_assets_in_pallet_selector.is_some()
            {
                break;
            }
        }
        let index_system_in_pallet_selector = index_system_in_pallet_selector.unwrap();
        let index_assets_in_pallet_selector = index_assets_in_pallet_selector.unwrap();

        let mut system_account_query = None;
        let mut assets_account_query = None;

        // System - Account
        storage_selector_functional = StorageSelectorFunctional::new_at::<(), RuntimeMetadataV15>(
            &storage_selector_functional.available_pallets,
            &mut (),
            &metadata_v15.types,
            index_system_in_pallet_selector,
        )
        .unwrap();

        if let EntrySelector::Functional(ref mut entry_selector_functional) =
            storage_selector_functional.query.entry_selector
        {
            let mut entry_index = None;
            for (index, entry) in entry_selector_functional
                .available_entries
                .iter()
                .enumerate()
            {
                if entry.name == "Account" {
                    entry_index = Some(index);
                    break;
                }
            }
            let entry_index = entry_index.unwrap();
            *entry_selector_functional = EntrySelectorFunctional::new_at::<(), RuntimeMetadataV15>(
                &entry_selector_functional.available_entries,
                &mut (),
                &metadata_v15.types,
                entry_index,
            )
            .unwrap();
            if let StorageEntryTypeToFill::Map {
                hashers: _,
                ref mut key_to_fill,
                value: _,
            } = entry_selector_functional.selected_entry.type_to_fill
            {
                if let TypeContentToFill::SpecialType(SpecialTypeToFill::AccountId32(
                    ref mut account_to_fill,
                )) = key_to_fill.content
                {
                    *account_to_fill = Some(substrate_parser::additional_types::AccountId32(account_id.0))
                }
            }

            system_account_query = storage_selector_functional.query.finalize().unwrap();
        }

        // Assets - Account
        storage_selector_functional = StorageSelectorFunctional::new_at::<(), RuntimeMetadataV15>(
            &storage_selector_functional.available_pallets,
            &mut (),
            &metadata_v15.types,
            index_assets_in_pallet_selector,
        )
        .unwrap();
        if let EntrySelector::Functional(ref mut entry_selector_functional) =
            storage_selector_functional.query.entry_selector
        {
            let mut entry_index = None;
            for (index, entry) in entry_selector_functional
                .available_entries
                .iter()
                .enumerate()
            {
                if entry.name == "Account" {
                    entry_index = Some(index);
                    break;
                }
            }
            let entry_index = entry_index.unwrap();
            *entry_selector_functional = EntrySelectorFunctional::new_at::<(), RuntimeMetadataV15>(
                &entry_selector_functional.available_entries,
                &mut (),
                &metadata_v15.types,
                entry_index,
            )
            .unwrap();
            if let StorageEntryTypeToFill::Map {
                hashers: _,
                ref mut key_to_fill,
                value: _,
            } = entry_selector_functional.selected_entry.type_to_fill
            {
                if let TypeContentToFill::Tuple(ref mut set) = key_to_fill.content {
                    for ty in set.iter_mut() {
                        match ty.content {
                            TypeContentToFill::SpecialType(SpecialTypeToFill::AccountId32(
                                ref mut account_to_fill,
                            )) => *account_to_fill = Some(substrate_parser::additional_types::AccountId32(account_id.0)),
                            TypeContentToFill::Primitive(PrimitiveToFill::CompactUnsigned(
                                ref mut specialty_unsigned_to_fill,
                            )) => {
                                if let UnsignedToFill::U32(ref mut u) =
                                    specialty_unsigned_to_fill.content
                                {
                                    *u = Some(asset_id);
                                }
                            }
                            TypeContentToFill::Primitive(PrimitiveToFill::Unsigned(
                                ref mut specialty_unsigned_to_fill,
                            )) => {
                                if let UnsignedToFill::U32(ref mut u) =
                                    specialty_unsigned_to_fill.content
                                {
                                    *u = Some(asset_id);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            assets_account_query = storage_selector_functional.query.finalize().unwrap();
        }

        return FinalizedQueries {
            system_account: system_account_query.unwrap(),
            assets_account: assets_account_query.unwrap(),
        };
    }
    panic!("was unable to find something");
}

pub fn derivations<'a>(recipient: &'a str, order: &'a str) -> FullDerivation<'a> {
    FullDerivation {
        junctions: vec![DeriveJunction::hard(recipient), DeriveJunction::hard(order)],
        password: None,
    }
}

pub fn hashed_key_element(data: &[u8], hasher: &StorageHasher) -> Vec<u8> {
    match hasher {
        StorageHasher::Blake2_128 => blake2_128(data).to_vec(),
        StorageHasher::Blake2_256 => blake2_256(data).to_vec(),
        StorageHasher::Blake2_128Concat => [blake2_128(data).to_vec(), data.to_vec()].concat(),
        StorageHasher::Twox128 => twox_128(data).to_vec(),
        StorageHasher::Twox256 => twox_256(data).to_vec(),
        StorageHasher::Twox64Concat => [twox_64(data).to_vec(), data.to_vec()].concat(),
        StorageHasher::Identity => data.to_vec(),
    }
}

pub fn whole_key_u32_value(
    prefix: &str,
    storage_name: &str,
    metadata_v15: &RuntimeMetadataV15,
    entered_data: u32,
) -> String {
    for pallet in metadata_v15.pallets.iter() {
        if let Some(storage) = &pallet.storage {
            if storage.prefix == prefix {
                for entry in storage.entries.iter() {
                    if entry.name == storage_name {
                        match &entry.ty {
                            StorageEntryType::Plain(_) => {
                                panic!("expected map with single entry, got plain")
                            }
                            StorageEntryType::Map {
                                hashers,
                                key: key_ty,
                                value: _,
                            } => {
                                if hashers.len() == 1 {
                                    let hasher = &hashers[0];
                                    match metadata_v15.types.resolve(key_ty.id).unwrap().type_def {
                                        TypeDef::Primitive(TypeDefPrimitive::U32) => {
                                            return format!(
                                                "0x{}{}{}",
                                                hex::encode(twox_128(prefix.as_bytes())),
                                                hex::encode(twox_128(storage_name.as_bytes())),
                                                hex::encode(hashed_key_element(
                                                    &entered_data.encode(),
                                                    hasher
                                                ))
                                            )
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
                panic!("have not found entry with proper name");
            }
        }
    }
    panic!("have not found pallet");
}

pub fn decimals(x: &Map<String, Value>) -> Result<u8, ErrorChain> {
    match x.get("tokenDecimals") {
        // decimals info is fetched in `system_properties` rpc call
        Some(a) => match a {
            // fetched decimals value is a number
            Value::Number(b) => match b.as_u64() {
                // number is integer and could be represented as `u64` (the only
                // suitable interpretation available for `Number`)
                Some(c) => match c.try_into() {
                    // this `u64` fits into `u8` that decimals is supposed to be
                    Ok(d) => Ok(d),

                    // this `u64` does not fit into `u8`, this is an error
                    Err(_) => Err(ErrorChain::DecimalsFormatNotSupported {
                        value: a.to_string(),
                    }),
                },

                // number could not be represented as `u64`, this is an error
                None => Err(ErrorChain::DecimalsFormatNotSupported {
                    value: a.to_string(),
                }),
            },

            // fetched decimals is an array
            Value::Array(b) => {
                // array with only one element
                if b.len() == 1 {
                    // this element is a number, process same as
                    // `Value::Number(_)`
                    if let Value::Number(c) = &b[0] {
                        match c.as_u64() {
                            // number is integer and could be represented as
                            // `u64` (the only suitable interpretation available
                            // for `Number`)
                            Some(d) => match d.try_into() {
                                // this `u64` fits into `u8` that decimals is
                                // supposed to be
                                Ok(f) => Ok(f),

                                // this `u64` does not fit into `u8`, this is an
                                // error
                                Err(_) => Err(ErrorChain::DecimalsFormatNotSupported {
                                    value: a.to_string(),
                                }),
                            },

                            // number could not be represented as `u64`, this is
                            // an error
                            None => Err(ErrorChain::DecimalsFormatNotSupported {
                                value: a.to_string(),
                            }),
                        }
                    } else {
                        // element is not a number, this is an error
                        Err(ErrorChain::DecimalsFormatNotSupported {
                            value: a.to_string(),
                        })
                    }
                } else {
                    // decimals are an array with more than one element
                    Err(ErrorChain::DecimalsFormatNotSupported {
                        value: a.to_string(),
                    })
                }
            }

            // unexpected decimals format
            _ => Err(ErrorChain::DecimalsFormatNotSupported {
                value: a.to_string(),
            }),
        },

        // decimals are missing
        None => Err(ErrorChain::NoDecimals),
    }
}

pub fn optional_prefix_from_meta(metadata: &RuntimeMetadataV15) -> Option<u16> {
    let mut base58_prefix_data = None;
    for pallet in &metadata.pallets {
        if pallet.name == "System" {
            for system_constant in &pallet.constants {
                if system_constant.name == "SS58Prefix" {
                    base58_prefix_data = Some((&system_constant.value, &system_constant.ty));
                    break;
                }
            }
            break;
        }
    }
    if let Some((value, ty_symbol)) = base58_prefix_data {
        match decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            ty_symbol,
            &value.as_ref(),
            &mut (),
            &metadata.types,
        ) {
            Ok(extended_data) => match extended_data.data {
                ParsedData::PrimitiveU8 {
                    value,
                    specialty: _,
                } => Some(value.into()),
                ParsedData::PrimitiveU16 {
                    value,
                    specialty: _,
                } => Some(value),
                ParsedData::PrimitiveU32 {
                    value,
                    specialty: _,
                } => value.try_into().ok(),
                ParsedData::PrimitiveU64 {
                    value,
                    specialty: _,
                } => value.try_into().ok(),
                ParsedData::PrimitiveU128 {
                    value,
                    specialty: _,
                } => value.try_into().ok(),
                _ => None,
            },
            Err(_) => None,
        }
    } else {
        None
    }
}

pub fn fetch_constant(
    metadata: &RuntimeMetadataV15,
    pallet_name: &str,
    constant_name: &str,
) -> Option<ExtendedData> {
    let mut found = None;
    for pallet in &metadata.pallets {
        if pallet.name == pallet_name {
            for constant in &pallet.constants {
                if constant.name == constant_name {
                    found = Some((&constant.value, &constant.ty));
                    break;
                }
            }
            break;
        }
    }
    if let Some((value, ty_symbol)) = found {
        decode_all_as_type::<&[u8], (), RuntimeMetadataV15>(
            ty_symbol,
            &value.as_ref(),
            &mut (),
            &metadata.types,
        )
        .ok()
    } else {
        None
    }
}

pub fn system_properties_to_short_specs(
    system_properties: &Map<String, Value>,
    metadata: &RuntimeMetadataV15,
) -> Result<ShortSpecs, ErrorChain> {
    let optional_prefix_from_meta = optional_prefix_from_meta(metadata);
    let base58prefix = base58prefix(system_properties, optional_prefix_from_meta)?;
    let decimals = decimals(system_properties)?;
    let unit = unit(system_properties)?;
    Ok(ShortSpecs {
        base58prefix,
        decimals,
        unit,
    })
}

pub fn pallet_index(metadata: &RuntimeMetadataV15, name: &str) -> Option<u8> {
    for pallet in &metadata.pallets {
        if pallet.name == name {
            return Some(pallet.index);
        }
    }
    return None;
}

pub fn storage_key(prefix: &str, storage_name: &str) -> String {
    format!(
        "0x{}{}",
        hex::encode(twox_128(prefix.as_bytes())),
        hex::encode(twox_128(storage_name.as_bytes()))
    )
}

pub fn base58prefix(
    x: &Map<String, Value>,
    optional_prefix_from_meta: Option<u16>,
) -> Result<u16, ErrorChain> {
    let base58prefix: u16 = match x.get("ss58Format") {
        // base58 prefix is fetched in `system_properties` rpc call
        Some(a) => match a {
            // base58 prefix value is a number
            Value::Number(b) => match b.as_u64() {
                // number is integer and could be represented as `u64` (the only
                // suitable interpretation available for `Number`)
                Some(c) => match c.try_into() {
                    // this `u64` fits into `u16` that base58 prefix is supposed
                    // to be
                    Ok(d) => match optional_prefix_from_meta {
                        // base58 prefix was found in `SS58Prefix` constant of
                        // the network metadata
                        //
                        // check that the prefixes match
                        Some(prefix_from_meta) => {
                            if prefix_from_meta == d {
                                d
                            } else {
                                return Err(ErrorChain::Base58PrefixMismatch {
                                    specs: d,
                                    meta: prefix_from_meta,
                                });
                            }
                        }

                        // no base58 prefix was found in the network metadata
                        None => d,
                    },

                    // `u64` value does not fit into `u16` base58 prefix format,
                    // this is an error
                    Err(_) => {
                        return Err(ErrorChain::Base58PrefixFormatNotSupported {
                            value: a.to_string(),
                        })
                    }
                },

                // base58 prefix value could not be presented as `u64` number,
                // this is an error
                None => {
                    return Err(ErrorChain::Base58PrefixFormatNotSupported {
                        value: a.to_string(),
                    })
                }
            },

            // base58 prefix value is not a number, this is an error
            _ => {
                return Err(ErrorChain::Base58PrefixFormatNotSupported {
                    value: a.to_string(),
                })
            }
        },

        // no base58 prefix fetched in `system_properties` rpc call
        None => match optional_prefix_from_meta {
            // base58 prefix was found in `SS58Prefix` constant of the network
            // metadata
            Some(prefix_from_meta) => prefix_from_meta,

            // no base58 prefix at all, this is an error
            None => return Err(ErrorChain::NoBase58Prefix),
        },
    };
    Ok(base58prefix)
}

pub fn unit(x: &Map<String, Value>) -> Result<String, ErrorChain> {
    match x.get("tokenSymbol") {
        // unit info is fetched in `system_properties` rpc call
        Some(a) => match a {
            // fetched unit value is a `String`
            Value::String(b) => {
                // definitive unit found
                Ok(b.to_string())
            }

            // fetched an array of units
            Value::Array(b) => {
                // array with a single element
                if b.len() == 1 {
                    // single `String` element array, process same as `String`
                    if let Value::String(c) = &b[0] {
                        // definitive unit found
                        Ok(c.to_string())
                    } else {
                        // element is not a `String`, this is an error
                        Err(ErrorChain::UnitFormatNotSupported {
                            value: a.to_string(),
                        })
                    }
                } else {
                    // units are an array with more than one element
                    Err(ErrorChain::UnitFormatNotSupported {
                        value: a.to_string(),
                    })
                }
            }

            // unexpected unit format
            _ => Err(ErrorChain::UnitFormatNotSupported {
                value: a.to_string(),
            }),
        },

        // unit missing
        None => Err(ErrorChain::NoUnit),
    }
}
