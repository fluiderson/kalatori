use crate::{AssetId, BlockNumber, Decimals, ExtrinsicIndex};
use anyhow::{Context, Result};
use axum::{
    extract::{rejection::RawPathParamsRejection, MatchedPath, Query, RawPathParams},
    http::{header, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Serialize, Serializer};
use std::{collections::HashMap, future::Future, net::SocketAddr};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub const MODULE: &str = module_path!();

const AMOUNT: &str = "amount";
const CURRENCY: &str = "currency";
const CALLBACK: &str = "callback";

#[derive(Serialize)]
struct OrderStatus {
    order: String,
    payment_status: PaymentStatus,
    message: String,
    recipient: String,
    server_info: ServerInfo,
    #[serde(skip_serializing_if = "Option::is_none", flatten)]
    order_info: Option<OrderInfo>,
}

#[derive(Serialize)]
struct OrderInfo {
    withdrawal_status: WithdrawalStatus,
    amount: f64,
    currency: CurrencyInfo,
    callback: String,
    transactions: Vec<TransactionInfo>,
    payment_account: String,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum PaymentStatus {
    Pending,
    Paid,
    Unknown,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum WithdrawalStatus {
    Waiting,
    Failed,
    Completed,
}

#[derive(Serialize)]
struct ServerStatus {
    description: ServerInfo,
    supported_currencies: Vec<CurrencyInfo>,
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
struct CurrencyInfo {
    currency: String,
    chain_name: String,
    kind: TokenKind,
    decimals: Decimals,
    rpc_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    asset_id: Option<AssetId>,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum TokenKind {
    Assets,
    Balances,
}

#[derive(Serialize)]
struct ServerInfo {
    version: &'static str,
    instance_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    debug: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kalatori_remark: Option<String>,
}

#[derive(Serialize)]
struct TransactionInfo {
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
) -> Result<impl Future<Output = Result<String>>> {
    let v2 = Router::new()
        .route("/order/:order_id", post(order))
        .route("/status", get(status))
        .route("/health", get(health));
    let app = Router::new().nest("/v2", v2);

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

fn process_order(
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
        // TODO: try to query an order from the database.

        Ok((
            OrderStatus {
                order,
                payment_status: PaymentStatus::Unknown,
                message: String::new(),
                recipient: String::new(),
                server_info: ServerInfo {
                    version: env!("CARGO_PKG_VERSION"),
                    instance_id: String::new(),
                    debug: None,
                    kalatori_remark: None,
                },
                order_info: None,
            },
            OrderSuccess::Found,
        ))
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

        // TODO: try to query & update or create an order in the database.

        if currency == "USDCT" {
            return Err(OrderError::UnknownCurrency);
        }

        if amount < 50.0 {
            return Err(OrderError::LessThanExistentialDeposit(50.0));
        }

        Ok((
            OrderStatus {
                order,
                payment_status: PaymentStatus::Pending,
                message: String::new(),
                recipient: String::new(),
                server_info: ServerInfo {
                    version: env!("CARGO_PKG_VERSION"),
                    instance_id: String::new(),
                    debug: None,
                    kalatori_remark: None,
                },
                order_info: Some(OrderInfo {
                    withdrawal_status: WithdrawalStatus::Waiting,
                    amount,
                    currency: CurrencyInfo {
                        currency,
                        chain_name: String::new(),
                        kind: TokenKind::Balances,
                        decimals: 0,
                        rpc_url: String::new(),
                        asset_id: None,
                    },
                    callback,
                    transactions: vec![],
                    payment_account: String::new(),
                }),
            },
            OrderSuccess::Created,
        ))
    }
}

async fn order(
    matched_path: MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    query: Query<HashMap<String, String>>,
) -> Response {
    match process_order(&matched_path, path_result, &query) {
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

async fn status() -> ([(HeaderName, &'static str); 1], Json<ServerStatus>) {
    (
        [(header::CACHE_CONTROL, "no-store")],
        ServerStatus {
            description: ServerInfo {
                version: env!("CARGO_PKG_VERSION"),
                instance_id: String::new(),
                debug: None,
                kalatori_remark: None,
            },
            supported_currencies: vec![CurrencyInfo {
                currency: String::new(),
                chain_name: String::new(),
                kind: TokenKind::Balances,
                decimals: 0,
                rpc_url: String::new(),
                asset_id: None,
            }],
        }
        .into(),
    )
}

async fn health() -> ([(HeaderName, &'static str); 1], Json<ServerHealth>) {
    (
        [(header::CACHE_CONTROL, "no-store")],
        ServerHealth {
            description: ServerInfo {
                version: env!("CARGO_PKG_VERSION"),
                instance_id: String::new(),
                debug: None,
                kalatori_remark: None,
            },
            connected_rpcs: [RpcInfo {
                rpc_url: String::new(),
                chain_name: String::new(),
                status: Health::Critical,
            }]
            .into(),
            status: Health::Degraded,
        }
        .into(),
    )
}
