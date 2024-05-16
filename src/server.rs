use crate::{
    definitions::api_v2::*,
    error::{Error, ErrorForceWithdrawal, ErrorOrder, ErrorServer},
    state::State,
};
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

pub async fn new(
    shutdown_notification: CancellationToken,
    host: SocketAddr,
    state: State,
) -> Result<impl Future<Output = Result<Cow<'static, str>, Error>>, ErrorServer> {
    let v2: Router<State> = Router::new()
        .route("/order/:order_id", routing::post(order))
        .route(
            "/order/:order_id/forceWithdrawal",
            routing::post(force_withdrawal),
        )
        .route("/status", routing::get(status))
        .route("/health", routing::get(health))
        .route("/audit", routing::get(audit))
        .route("/order/:order_id/investigate", routing::post(investigate));
    let app = Router::new()
        .route(
            "/public/v2/payment/:paymentAccount",
            routing::post(public_payment_account),
        )
        .nest("/v2", v2)
        .with_state(state);

    let listener = TcpListener::bind(host)
        .await
        .map_err(|_| ErrorServer::TcpListenerBind(host))?;

    Ok(async {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_notification.cancelled_owned())
            .await
            .map_err(|_| ErrorServer::ThreadError)?;

        Ok("The server module is shut down.".into())
    })
}

#[derive(Debug, Serialize)]
struct InvalidParameter {
    parameter: String,
    message: String,
}

async fn process_order(
    state: State,
    matched_path: &MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    query: &HashMap<String, String>,
) -> Result<OrderResponse, ErrorOrder> {
    const ORDER_ID: &str = "order_id";

    let path_parameters =
        path_result.map_err(|_| ErrorOrder::InvalidParameter(matched_path.as_str().to_owned()))?;
    let order = path_parameters
        .iter()
        .find_map(|(key, value)| (key == ORDER_ID).then_some(value))
        .ok_or_else(|| ErrorOrder::MissingParameter(ORDER_ID.into()))?
        .to_owned();

    if query.is_empty() {
        state
            .order_status(&order)
            .await
            .map_err(|_| ErrorOrder::InternalError)
    } else {
        let get_parameter = |parameter: &str| {
            query
                .get(parameter)
                .ok_or_else(|| ErrorOrder::MissingParameter(parameter.into()))
        };

        let currency = get_parameter(CURRENCY)?.to_owned();
        let callback = get_parameter(CALLBACK)?.to_owned();
        let amount = get_parameter(AMOUNT)?
            .parse()
            .map_err(|_| ErrorOrder::InvalidParameter(AMOUNT.into()))?;

        if currency != "USDC" {
            return Err(ErrorOrder::UnknownCurrency);
        }

        if amount < 0.07 {
            return Err(ErrorOrder::LessThanExistentialDeposit(0.07));
        }

        state
            .create_order(OrderQuery {
                order,
                amount,
                callback,
                currency,
            })
            .await
            .map_err(|_| ErrorOrder::InternalError)
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
        Ok(order) => match order {
            OrderResponse::NewOrder(order_status) => (StatusCode::CREATED, Json(order_status)).into_response(),
            OrderResponse::FoundOrder(order_status) => (StatusCode::OK, Json(order_status)).into_response(),
            OrderResponse::ModifiedOrder(order_status) => (StatusCode::OK, Json(order_status)).into_response(),
            OrderResponse::CollidedOrder(order_status) => (StatusCode::CONFLICT, Json(order_status)).into_response(),
            OrderResponse::NotFound => (StatusCode::NOT_FOUND, "").into_response(),
        },
        Err(error) => match error {
            ErrorOrder::LessThanExistentialDeposit(existential_deposit) => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter: AMOUNT.into(),
                    message: format!("provided amount is less than the currency's existential deposit ({existential_deposit})"),
                }]),
            )
                .into_response(),
            ErrorOrder::UnknownCurrency => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter: CURRENCY.into(),
                    message: "provided currency isn't supported".into(),
                }]),
            )
                .into_response(),
            ErrorOrder::MissingParameter(parameter) => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter,
                    message: "parameter wasn't found".into(),
                }]),
            )
                .into_response(),
            ErrorOrder::InvalidParameter(parameter) => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter,
                    message: "parameter's format is invalid".into(),
                }]),
            )
                .into_response(),
            ErrorOrder::InternalError => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
    }
}

async fn process_force_withdrawal(
    state: State,
    matched_path: &MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
) -> Result<OrderStatus, ErrorForceWithdrawal> {
    const ORDER_ID: &str = "order_id";

    let path_parameters = path_result
        .map_err(|_| ErrorForceWithdrawal::InvalidParameter(matched_path.as_str().to_owned()))?;
    let order = path_parameters
        .iter()
        .find_map(|(key, value)| (key == ORDER_ID).then_some(value))
        .ok_or_else(|| ErrorForceWithdrawal::MissingParameter(ORDER_ID.into()))?
        .to_owned();
    state
        .force_withdrawal(order)
        .await
        .map_err(ErrorForceWithdrawal::WithdrawalError)
}

#[debug_handler]
async fn force_withdrawal(
    extract::State(state): extract::State<State>,
    matched_path: MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
) -> Response {
    match process_force_withdrawal(state, &matched_path, path_result).await {
        Ok(a) => (StatusCode::CREATED, Json(a)).into_response(),
        Err(ErrorForceWithdrawal::WithdrawalError(a)) => {
            (StatusCode::BAD_REQUEST, Json(a)).into_response()
        }
        Err(ErrorForceWithdrawal::MissingParameter(parameter)) => (
            StatusCode::BAD_REQUEST,
            Json([InvalidParameter {
                parameter,
                message: "parameter wasn't found".into(),
            }]),
        )
            .into_response(),
        Err(ErrorForceWithdrawal::InvalidParameter(parameter)) => (
            StatusCode::BAD_REQUEST,
            Json([InvalidParameter {
                parameter,
                message: "parameter's format is invalid".into(),
            }]),
        )
            .into_response(),
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

async fn health(
    extract::State(state): extract::State<State>,
) -> ([(HeaderName, &'static str); 1], Json<ServerStatus>) {
    todo!();
}

async fn audit(extract::State(state): extract::State<State>) -> Response {
    StatusCode::NOT_IMPLEMENTED.into_response()
}

#[debug_handler]
async fn investigate(
    extract::State(state): extract::State<State>,
    matched_path: MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    query: Query<HashMap<String, String>>,
) -> Response {
    todo!()
}

#[debug_handler]
async fn public_payment_account(
    extract::State(state): extract::State<State>,
    matched_path: MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    query: Query<HashMap<String, String>>,
) -> Response {
    todo!()
}
