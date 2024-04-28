//! Deaf and dumb object definitions

use std::ops::Deref;

use serde::Deserialize;

pub type Version = u64;
pub type Nonce = u32;
pub type Timestamp = u64;
pub type PalletIndex = u8;

pub type BlockHash = primitive_types::H256;

pub type Entropy = Vec<u8>; // TODO: maybe enforce something here

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Chain {
    name: String,
    endpoints: Vec<String>,
    #[serde(flatten)]
    native_token: Option<NativeToken>,
    asset: Option<Vec<AssetInfo>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NativeToken {
    native_token: String,
    decimals: api_v2::Decimals,
}

#[derive(Deserialize)]
pub struct AssetInfo {
    name: String,
    id: api_v2::AssetId,
}

#[derive(Deserialize, Debug, Clone, Copy)]
pub struct Balance(u128);

impl Deref for Balance {
    type Target = u128;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Balance {
    pub fn format(&self, decimals: api_v2::Decimals) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let float = **self as f64;

        float / decimal_exponent_product(decimals)
    }

    pub fn parse(float: f64, decimals: api_v2::Decimals) -> Self {
        let parsed_float = (float * decimal_exponent_product(decimals)).round();

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self(parsed_float as _)
    }
}

pub fn decimal_exponent_product(decimals: api_v2::Decimals) -> f64 {
    10f64.powi(decimals.into())
}

/// Self-sufficient schemas used by Api v2.0.0
pub mod api_v2 {

    use std::collections::HashMap;

    use parity_scale_codec::{Decode, Encode};
    use serde::{Serialize, Serializer};

    pub const AMOUNT: &str = "amount";
    pub const CURRENCY: &str = "currency";
    pub const CALLBACK: &str = "callback";

    pub type AssetId = u32;
    pub type Decimals = u8;
    pub type BlockNumber = u64;
    pub type ExtrinsicIndex = u32;

    #[derive(Debug)]
    pub struct OrderQuery {
        pub order: String,
        pub amount: f64,
        pub callback: String,
        pub currency: String,
    }

    #[derive(Debug)]
    pub enum OrderResponse {
        NewOrder(OrderStatus),
        FoundOrder(OrderStatus),
        ModifiedOrder(OrderStatus),
        CollidedOrder(OrderStatus),
        NotFound,
    }

    #[derive(Debug, Serialize)]
    pub struct OrderStatus {
        pub order: String,
        pub message: String,
        pub recipient: String,
        pub server_info: ServerInfo,
        #[serde(flatten)]
        pub order_info: OrderInfo,
    }

    #[derive(Clone, Debug, Serialize, Encode, Decode)]
    pub struct OrderInfo {
        pub withdrawal_status: WithdrawalStatus,
        pub payment_status: PaymentStatus,
        pub amount: f64,
        pub currency: CurrencyInfo,
        pub callback: String,
        pub transactions: Vec<TransactionInfo>,
        pub payment_account: String,
    }

    impl OrderInfo {
        pub fn new(query: OrderQuery, currency: CurrencyInfo, payment_account: String) -> Self {
            OrderInfo {
                withdrawal_status: WithdrawalStatus::Waiting,
                payment_status: PaymentStatus::Pending,
                amount: query.amount,
                currency,
                callback: query.callback,
                transactions: Vec::new(),
                payment_account,
            }
        }
    }

    pub enum OrderCreateResponse {
        New,
        Modified,
        Collision(OrderInfo),
    }

    #[derive(Clone, Debug, Serialize, Decode, Encode, PartialEq)]
    #[serde(rename_all = "lowercase")]
    pub enum PaymentStatus {
        Pending,
        Paid,
    }

    #[derive(Clone, Debug, Serialize, Decode, Encode, PartialEq)]
    #[serde(rename_all = "lowercase")]
    pub enum WithdrawalStatus {
        Waiting,
        Failed,
        Completed,
    }

    #[derive(Clone, Debug, Serialize)]
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

    #[derive(Clone, Debug, Serialize, Decode, Encode)]
    pub struct CurrencyInfo {
        pub currency: String,
        pub chain_name: String,
        pub kind: TokenKind,
        pub decimals: Decimals,
        pub rpc_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub asset_id: Option<AssetId>,
        #[serde(skip_serializing)]
        pub ss58: u16,
    }

    impl CurrencyInfo {
        pub fn properties(&self) -> CurrencyProperties {
            CurrencyProperties {
                chain_name: self.chain_name.clone(),
                kind: self.kind,
                decimals: self.decimals,
                rpc_url: self.rpc_url.clone(),
                asset_id: self.asset_id,
                ss58: self.ss58,
            }
        }
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct CurrencyProperties {
        pub chain_name: String,
        pub kind: TokenKind,
        pub decimals: Decimals,
        pub rpc_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub asset_id: Option<AssetId>,
        #[serde(skip_serializing)]
        pub ss58: u16,
    }

    impl CurrencyProperties {
        pub fn info(&self, currency: String) -> CurrencyInfo {
            CurrencyInfo {
                currency,
                chain_name: self.chain_name.clone(),
                kind: self.kind,
                decimals: self.decimals,
                rpc_url: self.rpc_url.clone(),
                asset_id: self.asset_id,
                ss58: self.ss58,
            }
        }
    }

    #[derive(Clone, Copy, Debug, Serialize, Decode, Encode)]
    #[serde(rename_all = "lowercase")]
    pub enum TokenKind {
        Asset,
        Balances,
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct ServerInfo {
        pub version: &'static str,
        pub instance_id: String,
        pub debug: bool,
        pub kalatori_remark: String,
    }

    #[derive(Clone, Debug, Serialize, Decode, Encode)]
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

    #[derive(Clone, Debug, Serialize, Decode, Encode)]
    struct FinalizedTx {
        block_number: BlockNumber,
        position_in_block: ExtrinsicIndex,
        timestamp: String,
    }

    #[derive(Clone, Debug, Decode, Encode)]
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

    #[derive(Clone, Debug, Serialize, Decode, Encode)]
    #[serde(rename_all = "lowercase")]
    enum TxStatus {
        Pending,
        Finalized,
        Failed,
    }
}

#[cfg(test)]
#[test]
#[allow(
    clippy::inconsistent_digit_grouping,
    clippy::unreadable_literal,
    clippy::float_cmp
)]

fn balance_insufficient_precision() {
    const DECIMALS: Decimals = 10;

    let float = 931395.862219815_3;
    let parsed = Balance::parse(float, DECIMALS);

    assert_eq!(*parsed, 931395_862219815_2);
    assert_eq!(parsed.format(DECIMALS), 931395.862219815_1);
}
