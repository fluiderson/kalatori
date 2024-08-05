//! Substrate RPC API functions & calls.
//!
//! - <https://paritytech.github.io/json-rpc-interface-spec/api.html>
//! - <https://docs.rs/sc-rpc-api>

use crate::{chain::definitions::BlockHash, error::ApiError};
use jsonrpsee::{core::client::ClientT, ws_client::WsClient};

pub async fn fetch_genesis_hash(client: &WsClient) -> Result<BlockHash, ApiError> {
    client
        .request("chain_getBlockHash", [0])
        .await
        .map_err(ApiError::GenesisHash)
}

// pub async fn fetch_finalized_head(client: &WsClient) -> Result<BlockHash, ApiError> {
//     client
//         .request("chain_getFinalizedHead", jsonrpsee::rpc_params![])
//         .await
//         .map_err(ApiError::FinalizedHead)
// }

// pub async fn fetch_metadata(
//     client: &WsClient,
//     block: &BlockHash,
// ) -> Result<RuntimeMetadataV14, ApiError> {
//     let bytes: Bytes = client
//         .request("state_getMetadata", [block])
//         .await
//         .map_err(ApiError::FinalizedHead)?;
//     let prefixed = RuntimeMetadataPrefixed::decode(&mut &**bytes)?;

//     if prefixed.0 != META_RESERVED {
//         return Err(ApiError::UnexpectedMetadataPrefix);
//     }

//     if let RuntimeMetadata::V14(metadata) = prefixed.1 {
//         Ok(metadata)
//     } else {
//         Err(ApiError::UnsupportedMetadataVersion)
//     }
// }

// pub fn runv(metadata: &RuntimeMetadataV14) {
//     let d = metadata
//         .pallets
//         .iter()
//         .find(|pallet| pallet.name == "System")
//         .unwrap()
//         .constants
//         .iter()
//         .find(|constant| constant.name == "Version")
//         .unwrap();

//     let a = substrate_parser::decode_all_as_type::<_, _, RuntimeMetadataV14>(
//         &d.ty,
//         &&*d.value,
//         &mut (),
//         &metadata.types,
//     ).unwrap();

//     println!("{a:#?}");
// }

// pub fn extract_constant<T: DecodeAsType>(
//     metadata: &RuntimeMetadataV14,
//     constant_into: impl Into<Constant>,
// ) -> Result<T, ApiError> {
//     let constant = constant_into.into();
//     let (expected_pallet, expected_constant) = constant.pallet_n_constant();

//     let Some(pallet) = metadata
//         .pallets
//         .iter()
//         .find(|pallet| pallet.name == expected_pallet.to_str())
//     else {
//         return Err(ApiError::PalletNotFound(expected_pallet));
//     };

//     let Some(pallet_constant) = pallet
//         .constants
//         .iter()
//         .find(|pallet_constant| pallet_constant.name == expected_constant)
//     else {
//         return Err(ApiError::ConstantNotFound(constant));
//     };

//     // let parsed = substrate_parser::decode_all_as_type::<_, _, RuntimeMetadataV14>(
//     //     &pallet_constant.ty,
//     //     &&*pallet_constant.value,
//     //     &mut (),
//     //     &metadata.types,
//     // )?.data;

//     // Ok(T::decode(parsed).unwrap())

//     Ok(T::decode_as_type(
//         &mut &*pallet_constant.value,
//         pallet_constant.ty.id,
//         &metadata.types,
//     )
//     .unwrap())
//     // Ok(Value::decode_as_type(
//     //     &mut &*pallet_constant.value,
//     //     pallet_constant.ty.id,
//     //     &metadata.types,
//     // )
//     // .unwrap())
// }

// pub async fn fetch_runtime_version(
//     client: &WsClient,
//     block: &BlockHash,
// ) -> Result<RuntimeVersion, ApiError> {
//     client
//         .request("chain_getRuntimeVersion", [block])
//         .await
//         .map_err(ApiError::RuntimeVersion)
// }
