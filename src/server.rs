use crate::{
    database::{State}, AssetId, BlockNumber, Decimals, ExtrinsicIndex,
};
use anyhow::{Context, Result};
use axum::{
    extract::{self, rejection::RawPathParamsRejection, MatchedPath, Query, RawPathParams},
    http::{header, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing, Json, Router,
};
use axum_macros::debug_handler;
use serde::{Serialize, Serializer};
use std::{borrow::Cow, collections::HashMap, future::Future, net::SocketAddr};

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub const MODULE: &str = module_path!();

const AMOUNT: &str = "amount";
const CURRENCY: &str = "currency";
const CALLBACK: &str = "callback";

pub struct OrderQuery {
    pub order: String,
    pub amount: f64,
    pub callback: String,
    pub currency: String,
}

#[derive(Serialize)]
pub struct OrderStatus {
    pub order: String,
    pub payment_status: PaymentStatus,
    pub message: String,
    pub recipient: String,
    pub server_info: ServerInfo,
    #[serde(flatten)]
    pub order_info: OrderInfo,
}

#[derive(Serialize)]
pub struct OrderInfo {
    pub withdrawal_status: WithdrawalStatus,
    pub amount: f64,
    pub currency: CurrencyInfo,
    pub callback: String,
    pub transactions: Vec<TransactionInfo>,
    pub payment_account: String,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentStatus {
    Pending,
    Paid,
    Unknown,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WithdrawalStatus {
    Waiting,
    Failed,
    Completed,
}

#[derive(Serialize)]
pub struct ServerStatus {
    description: ServerInfo,
    supported_currencies: HashMap<std::string::String, CurrencyProperties>,
}

#[derive(Serialize)]
struct ServerHealth {
    description: ServerInfo,
    connected_rpcs: Vec<RpcInfo>,
    status: Health,
}

#[derive(Serialize)]
struct RpcInfo {
    rpc_url: String,
    chain_name: String,
    status: Health,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Health {
    Ok,
    Degraded,
    Critical,
}

#[derive(Serialize)]
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

#[derive(Serialize)]
pub struct ServerInfo {
    pub version: &'static str,
    pub instance_id: String,
    pub debug: bool,
    pub kalatori_remark: String,
}

#[derive(Serialize)]
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

#[derive(Serialize)]
struct FinalizedTx {
    block_number: BlockNumber,
    position_in_block: ExtrinsicIndex,
    timestamp: String,
}

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

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum TxStatus {
    Pending,
    Finalized,
    Failed,
}

pub async fn new(
    shutdown_notification: CancellationToken,
    host: SocketAddr,
    state: State,
) -> Result<impl Future<Output = Result<Cow<'static, str>>>> {
    let v2: Router<State> = Router::new()
        .route("/order/:order_id", routing::post(order))
        .route("/status", routing::get(status));
    let app = Router::new().nest("/v2", v2).with_state(state);

    let listener = TcpListener::bind(host)
        .await
        .with_context(|| format!("failed to bind the TCP listener to {host:?}"))?;

    Ok(async {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_notification.cancelled_owned())
            .await?;

        Ok("The server module is shut down.".into())
    })
}

enum OrderSuccess {
    Created,
    Found,
}

enum OrderError {
    LessThanExistentialDeposit(f64),
    UnknownCurrency,
    MissingParameter(String),
    InvalidParameter(String),
    AlreadyProcessed(Box<OrderStatus>),
    InternalError,
}

#[derive(Serialize)]
struct InvalidParameter {
    parameter: String,
    message: String,
}

async fn process_order(
    state: State,
    matched_path: &MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    query: &HashMap<String, String>,
) -> Result<(OrderStatus, OrderSuccess), OrderError> {
    const ORDER_ID: &str = "order_id";

    let path_parameters =
        path_result.map_err(|_| OrderError::InvalidParameter(matched_path.as_str().to_owned()))?;
    let order = path_parameters
        .iter()
        .find_map(|(key, value)| (key == ORDER_ID).then_some(value))
        .ok_or_else(|| OrderError::MissingParameter(ORDER_ID.into()))?
        .to_owned();

    if query.is_empty() {
        let order_status = state
            .order_status(&order)
            .await
            .map_err(|_| OrderError::InternalError)?;
        Ok((order_status, OrderSuccess::Found))
    } else {
        let get_parameter = |parameter: &str| {
            query
                .get(parameter)
                .ok_or_else(|| OrderError::MissingParameter(parameter.into()))
        };

        let currency = get_parameter(CURRENCY)?.to_owned();
        let callback = get_parameter(CALLBACK)?.to_owned();
        let amount = get_parameter(AMOUNT)?
            .parse()
            .map_err(|_| OrderError::InvalidParameter(AMOUNT.into()))?;

        if currency != "USDC" {
            return Err(OrderError::UnknownCurrency);
        }

        if amount < 0.07 {
            return Err(OrderError::LessThanExistentialDeposit(0.07));
        }

        let order_status = state
            .create_order(OrderQuery {
                order,
                amount,
                callback,
                currency,
            })
            .await
            .map_err(|_| OrderError::InternalError)?;

        Ok((order_status, OrderSuccess::Created))
    }
}

#[debug_handler]
async fn order(
    extract::State(state): extract::State<State>,
    matched_path: MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    query: Query<HashMap<String, String>>,
) -> Response {
    match process_order(state, &matched_path, path_result, &query).await {
        Ok((order_status, order_success)) => match order_success {
            OrderSuccess::Created => (StatusCode::CREATED, Json(order_status)),
            OrderSuccess::Found => (StatusCode::OK, Json(order_status)),
        }
        .into_response(),
        Err(error) => match error {
            OrderError::LessThanExistentialDeposit(existential_deposit) => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter: AMOUNT.into(),
                    message: format!("provided amount is less than the currency's existential deposit ({existential_deposit})"),
                }]),
            )
                .into_response(),
            OrderError::UnknownCurrency => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter: CURRENCY.into(),
                    message: "provided currency isn't supported".into(),
                }]),
            )
                .into_response(),
            OrderError::MissingParameter(parameter) => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter,
                    message: "parameter wasn't found".into(),
                }]),
            )
                .into_response(),
            OrderError::InvalidParameter(parameter) => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter,
                    message: "parameter's format is invalid".into(),
                }]),
            )
                .into_response(),
            OrderError::AlreadyProcessed(order_status) => {
                (StatusCode::CONFLICT, Json(order_status)).into_response()
            }
            OrderError::InternalError => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
    }
}

async fn status(
    extract::State(state): extract::State<State>,
) -> ([(HeaderName, &'static str); 1], Json<ServerStatus>) {
    match state.server_status().await {
        Ok(status) => ([(header::CACHE_CONTROL, "no-store")], status.into()),
        Err(_e) => panic!("db connection is down, state is lost"), //TODO tell this to client
    }
}
