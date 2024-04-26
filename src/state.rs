use crate::{
    database::Database,
    definitions::api_v2::{CurrencyProperties, OrderQuery, OrderResponse, OrderStatus, ServerInfo, ServerStatus},
    error::Error,
    ConfigWoChains, TaskTracker,
};

use std::collections::HashMap;

use substrate_crypto_light::common::AccountId32;
use substrate_crypto_light::sr25519::Pair;
use tokio::sync::oneshot;

/// Struct to store state of daemon. If something requires cooperation of more than one component,
/// it should go through here.
#[derive(Clone, Debug)]
pub struct State {
    pub tx: tokio::sync::mpsc::Sender<StateAccessRequest>,
}

impl State {
    pub fn initialise(
        currencies: HashMap<String, CurrencyProperties>,
        current_pair: Pair,
        old_pairs: HashMap<String, Pair>,
        ConfigWoChains {
            recipient,
            debug,
            remark,
            depth,
            account_lifetime,
            rpc,
        }: ConfigWoChains,
        db: Database,
        instance_id: String,
        task_tracker: TaskTracker,
    ) -> Result<Self, Error> {
        /*
            currencies: HashMap<String, CurrencyProperties>,
            recipient: AccountId,
            pair: Pair,
            depth: Option<Timestamp>,
            account_lifetime: Timestamp,
            debug: bool,
            remark: String,
            invoices: RwLock<HashMap<String, Invoicee>>,
            rpc: String,
        */
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);

        let recipient_ss58 = String::new(); // TODO ASAP

        // Remember to always spawn async here or things might deadlock
        task_tracker.spawn("State Handler", async move {
            while let Some(request) = rx.recv().await {
                match request {
                    StateAccessRequest::GetInvoiceStatus(request) => {
                        let server_info = ServerInfo { // TODO
                            version: env!("CARGO_PKG_VERSION"),
                            instance_id: instance_id.clone(),
                            debug,
                            kalatori_remark: remark.clone(),
                        };

                        request.res.send(get_invoice_status(request.order, recipient_ss58.clone(), server_info, &db).await);
                    }
                    StateAccessRequest::CreateInvoice(a) => {}
                    StateAccessRequest::ServerStatus(res) => {
                        let description = ServerInfo {
                            version: env!("CARGO_PKG_VERSION"),
                            instance_id: String::new(),
                            debug,
                            kalatori_remark: remark.clone(),
                        };
                        let server_status = ServerStatus {
                            description,
                            supported_currencies: currencies.clone(),
                        };
                        res.send(server_status);
                    }
                };
            }

            Ok("State handler is shutting down".into())
        });

        Ok(Self { tx })
    }

    pub async fn order_status(&self, order: &str) -> Result<OrderResponse, Error> {
        let (res, rx) = oneshot::channel();
        self.tx
            .send(StateAccessRequest::GetInvoiceStatus(GetInvoiceStatus {
                order: order.to_string(),
                res,
            }))
            .await;
        rx.await.map_err(|_| Error::Fatal)?
    }

    pub async fn server_status(&self) -> Result<ServerStatus, Error> {
        let (res, rx) = oneshot::channel();
        self.tx.send(StateAccessRequest::ServerStatus(res)).await;
        rx.await.map_err(|_| Error::Fatal)
    }

    pub async fn create_order(&self, order_query: OrderQuery) -> Result<OrderResponse, Error> {
        let (res, rx) = oneshot::channel();
        /*
                Invoicee {
                        callback: callback.clone(),
                        amount: Balance::parse(amount, 6),
                        paid: false,
                        paym_acc: pay_acc.clone(),
                    },
        */
        self.tx
            .send(StateAccessRequest::CreateInvoice(CreateInvoice {
                order_query,
                res,
            }))
            .await;
        rx.await.map_err(|_| Error::Fatal)?
    }

    pub fn interface(&self) -> Self {
        State {
            tx: self.tx.clone(),
        }
    }
}

enum StateAccessRequest {
    GetInvoiceStatus(GetInvoiceStatus),
    CreateInvoice(CreateInvoice),
    ServerStatus(oneshot::Sender<ServerStatus>),
}

struct GetInvoiceStatus {
    pub order: String,
    pub res: oneshot::Sender<Result<OrderResponse, Error>>,
}

struct CreateInvoice {
    pub order_query: OrderQuery,
    pub res: oneshot::Sender<Result<OrderResponse, Error>>,
}

async fn get_invoice_status(order: String, recipient: String, server_info: ServerInfo, db: &Database) -> Result<OrderResponse, Error> {
    if let Some(order_info) = db.read_order(order.clone()).await? {
        let message = String::new(); //TODO
        Ok(OrderResponse::FoundOrder(OrderStatus {
            order,
            message,
            recipient,
            server_info,
            order_info,
       }))
    } else {Ok(OrderResponse::NotFound)}
}
