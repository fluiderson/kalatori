use crate::{database::State, AccountId, AssetId, Balance, BlockNumber, Decimals, ExtrinsicIndex};
use anyhow::{Context, Result};
use axum::{
    extract::{self, rejection::RawPathParamsRejection, MatchedPath, Query, RawPathParams},
    http::{header, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing, Json, Router,
};
use serde::{Serialize, Serializer};
use std::{borrow::Cow, collections::HashMap, future::Future, net::SocketAddr, sync::Arc};
use subxt::ext::sp_core::{crypto::Ss58Codec, DeriveJunction, Pair};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub const MODULE: &str = module_path!();

const AMOUNT: &str = "amount";
const CURRENCY: &str = "currency";
const CALLBACK: &str = "callback";

#[derive(Serialize)]
pub struct OrderStatus {
    pub order: String,
    pub payment_status: PaymentStatus,
    pub message: String,
    pub recipient: String,
    pub server_info: ServerInfo,
    #[serde(skip_serializing_if = "Option::is_none", flatten)]
    pub order_info: Option<OrderInfo>,
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
pub struct CurrencyInfo {
    pub currency: String,
    pub chain_name: String,
    pub kind: TokenKind,
    pub decimals: Decimals,
    pub rpc_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    Asset,
    Balances,
}

#[derive(Serialize)]
pub struct ServerInfo {
    pub version: &'static str,
    pub instance_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kalatori_remark: Option<String>,
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

// pub async fn new(
//     shutdown_notification: CancellationToken,
//     host: SocketAddr,
//     state: Arc<State>,
// ) -> Result<impl Future<Output = Result<Cow<'static, str>>>> {
//     let v2 = Router::new()
//         .route("/order/:order_id", routing::post(order))
//         .route("/status", routing::get(status));
//     let app = Router::new().nest("/v2", v2).with_state(state);

//     let listener = TcpListener::bind(host)
//         .await
//         .with_context(|| format!("failed to bind the TCP listener to {host:?}"))?;

//     Ok(async {
//         axum::serve(listener, app)
//             .with_graceful_shutdown(shutdown_notification.cancelled_owned())
//             .await?;

//         Ok("The server module is shut down.".into())
//     })
// }

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

#[allow(clippy::too_many_lines)]
async fn process_order(
    state: extract::State<Arc<State>>,
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

        // let invoices = state.0.invoices.read().await;

        // if let Some(invoice) = invoices.get(&order) {
        //     Ok((
        //         OrderStatus {
        //             order,
        //             payment_status: if invoice.paid {
        //                 PaymentStatus::Paid
        //             } else {
        //                 PaymentStatus::Unknown
        //             },
        //             message: String::new(),
        //             recipient: state.0.recipient.to_ss58check(),
        //             server_info: ServerInfo {
        //                 version: env!("CARGO_PKG_VERSION"),
        //                 instance_id: String::new(),
        //                 debug: state.0.debug,
        //                 kalatori_remark: state.remark.clone(),
        //             },
        //             order_info: Some(OrderInfo {
        //                 withdrawal_status: WithdrawalStatus::Waiting,
        //                 amount: invoice.amount.format(6),
        //                 currency: CurrencyInfo {
        //                     currency: "USDC".into(),
        //                     chain_name: "assethub-polkadot".into(),
        //                     kind: TokenKind::Asset,
        //                     decimals: 6,
        //                     rpc_url: state.rpc.clone(),
        //                     asset_id: Some(1337),
        //                 },
        //                 callback: invoice.callback.clone(),
        //                 transactions: vec![],
        //                 payment_account: invoice.paym_acc.to_ss58check(),
        //             }),
        //         },
        //         OrderSuccess::Found,
        //     ))
        // } else {
        //     Ok((
        //         OrderStatus {
        //             order,
        //             payment_status: PaymentStatus::Unknown,
        //             message: String::new(),
        //             recipient: state.0.recipient.to_ss58check(),
        //             server_info: ServerInfo {
        //                 version: env!("CARGO_PKG_VERSION"),
        //                 instance_id: String::new(),
        //                 debug: state.0.debug,
        //                 kalatori_remark: state.remark.clone(),
        //             },
        //             order_info: None,
        //         },
        //         OrderSuccess::Found,
        //     ))
        // }

        todo!()
    } else {
        let get_parameter = |parameter: &str| {
            query
                .get(parameter)
                .ok_or_else(|| OrderError::MissingParameter(parameter.into()))
        };

        let currency = get_parameter(CURRENCY)?.to_owned();
        let callback = get_parameter(CALLBACK)?.to_owned();
        // let amount = get_parameter(AMOUNT)?
        //     .parse()
        //     .map_err(|_| OrderError::InvalidParameter(AMOUNT.into()))?;

        // TODO: try to query & update or create an order in the database.

        if currency != "USDC" {
            return Err(OrderError::UnknownCurrency);
        }

        // if amount < 0.07 {
        //     return Err(OrderError::LessThanExistentialDeposit(0.07));
        // }

        // let mut invoices = /* state.0.invoices.write().await */ todo!();
        let pay_acc: AccountId = /* state
            .0
            .pair
            .derive(vec![DeriveJunction::hard(order.clone())].into_iter(), None)
            .unwrap()
            .0
            .public()
            .into() */ todo!();

        // invoices.insert(
        //     order.clone(),
        //     Invoicee {
        //         callback: callback.clone(),
        //         amount: Balance::parse(amount, 6),
        //         paid: false,
        //         paym_acc: pay_acc.clone(),
        //     },
        // );

        // Ok((
        //     OrderStatus {
        //         order,
        //         payment_status: PaymentStatus::Pending,
        //         message: String::new(),
        //         recipient: state.0.recipient.to_ss58check(),
        //         server_info: ServerInfo {
        //             version: env!("CARGO_PKG_VERSION"),
        //             instance_id: String::new(),
        //             debug: state.0.debug,
        //             kalatori_remark: state.0.remark.clone(),
        //         },
        //         order_info: Some(OrderInfo {
        //             withdrawal_status: WithdrawalStatus::Waiting,
        //             amount,
        //             currency: CurrencyInfo {
        //                 currency: "USDC".into(),
        //                 chain_name: "assethub-polkadot".into(),
        //                 kind: TokenKind::Asset,
        //                 decimals: 6,
        //                 rpc_url: state.rpc.clone(),
        //                 asset_id: Some(1337),
        //             },
        //             callback,
        //             transactions: vec![],
        //             payment_account: pay_acc.to_ss58check(),
        //         }),
        //     },
        //     OrderSuccess::Created,
        // ))
        todo!()
    }
}

async fn order(
    state: extract::State<Arc<State>>,
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
    state: extract::State<Arc<State>>,
) -> ([(HeaderName, &'static str); 1], Json<ServerStatus>) {
    (
        [(header::CACHE_CONTROL, "no-store")],
        // ServerStatus {
        //     description: ServerInfo {
        //         version: env!("CARGO_PKG_VERSION"),
        //         instance_id: String::new(),
        //         debug: state.0.debug,
        //         kalatori_remark: state.0.remark.clone(),
        //     },
        //     supported_currencies: vec![CurrencyInfo {
        //         currency: "USDC".into(),
        //         chain_name: "assethub-polkadot".into(),
        //         kind: TokenKind::Asset,
        //         decimals: 6,
        //         rpc_url: state.rpc.clone(),
        //         asset_id: Some(1337),
        //     }],
        // }
        // .into(),
        todo!(),
    )
}
