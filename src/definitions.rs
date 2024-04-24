//! Deaf and dumb object definitions

pub mod api_v2 {
    use crate::{AssetId, BlockNumber, Decimals, ExtrinsicIndex};

    use std::collections::HashMap;

    use serde::{Serialize, Serializer};

    pub const AMOUNT: &str = "amount";
    pub const CURRENCY: &str = "currency";
    pub const CALLBACK: &str = "callback";

    #[derive(Debug)]
    pub struct OrderQuery {
        pub order: String,
        pub amount: f64,
        pub callback: String,
        pub currency: String,
    }

    #[derive(Debug, Serialize)]
    pub struct OrderStatus {
        pub order: String,
        pub payment_status: PaymentStatus,
        pub message: String,
        pub recipient: String,
        pub server_info: ServerInfo,
        #[serde(flatten)]
        pub order_info: OrderInfo,
    }

    #[derive(Debug, Serialize)]
    pub struct OrderInfo {
        pub withdrawal_status: WithdrawalStatus,
        pub amount: f64,
        pub currency: CurrencyInfo,
        pub callback: String,
        pub transactions: Vec<TransactionInfo>,
        pub payment_account: String,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "lowercase")]
    pub enum PaymentStatus {
        Pending,
        Paid,
        Unknown,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "lowercase")]
    pub enum WithdrawalStatus {
        Waiting,
        Failed,
        Completed,
    }

    #[derive(Debug, Serialize)]
    pub struct ServerStatus {
        pub description: ServerInfo,
        pub supported_currencies: HashMap<std::string::String, CurrencyProperties>,
    }

    #[derive(Debug, Serialize)]
    struct ServerHealth {
        description: ServerInfo,
        connected_rpcs: Vec<RpcInfo>,
        status: Health,
    }

    #[derive(Debug, Serialize)]
    struct RpcInfo {
        rpc_url: String,
        chain_name: String,
        status: Health,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "lowercase")]
    enum Health {
        Ok,
        Degraded,
        Critical,
    }

    #[derive(Debug, Serialize)]
    pub struct CurrencyInfo {
        pub currency: String,
        pub chain_name: String,
        pub kind: TokenKind,
        pub decimals: Decimals,
        pub rpc_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub asset_id: Option<AssetId>,
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct CurrencyProperties {
        pub chain_name: String,
        pub kind: TokenKind,
        pub decimals: Decimals,
        pub rpc_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub asset_id: Option<AssetId>,
    }

    #[derive(Clone, Copy, Debug, Serialize)]
    #[serde(rename_all = "lowercase")]
    pub enum TokenKind {
        Asset,
        Balances,
    }

    #[derive(Debug, Serialize)]
    pub struct ServerInfo {
        pub version: &'static str,
        pub instance_id: String,
        pub debug: bool,
        pub kalatori_remark: String,
    }

    #[derive(Debug, Serialize)]
    pub struct TransactionInfo {
        #[serde(skip_serializing_if = "Option::is_none", flatten)]
        finalized_tx: Option<FinalizedTx>,
        transaction_bytes: String,
        sender: String,
        recipient: String,
        #[serde(serialize_with = "amount_serializer")]
        amount: Amount,
        currency: CurrencyInfo,
        status: TxStatus,
    }

    #[derive(Debug, Serialize)]
    struct FinalizedTx {
        block_number: BlockNumber,
        position_in_block: ExtrinsicIndex,
        timestamp: String,
    }

    #[derive(Debug)]
    enum Amount {
        All,
        Exact(f64),
    }

    fn amount_serializer<S: Serializer>(amount: &Amount, serializer: S) -> Result<S::Ok, S::Error> {
        match amount {
            Amount::All => serializer.serialize_str("all"),
            Amount::Exact(exact) => exact.serialize(serializer),
        }
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "lowercase")]
    enum TxStatus {
        Pending,
        Finalized,
        Failed,
    }
}
