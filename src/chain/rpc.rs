use crate::{chain::definitions::BlockHash, error::RpcError};
use jsonrpsee::{
    core::client::ClientT,
    ws_client::{PingConfig, WsClient, WsClientBuilder},
};

pub async fn connect(url: impl AsRef<str>) -> Result<WsClient, RpcError> {
    WsClientBuilder::new()
        .enable_ws_ping(PingConfig::new())
        .build(url)
        .await
        .map_err(RpcError::Connection)
}

pub async fn fetch_genesis_hash(client: &WsClient) -> Result<BlockHash, RpcError> {
    client
        .request("chain_getBlockHash", [0])
        .await
        .map_err(RpcError::GenesisHash)
}
