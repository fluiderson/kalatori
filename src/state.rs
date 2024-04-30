use crate::{
    chain::derivations,
    database::Database,
    definitions::{
        api_v2::{
            CurrencyProperties, OrderCreateResponse, OrderInfo, OrderQuery, OrderResponse,
            OrderStatus, ServerInfo, ServerStatus,
        },
        Entropy,
    },
    error::{Error, ErrorOrder},
    rpc::ChainManager,
    ConfigWoChains, TaskTracker,
};

use std::collections::HashMap;

use substrate_crypto_light::common::{AccountId32, AsBase58};
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
        seed_entropy: Entropy,
        ConfigWoChains {
            recipient,
            debug,
            remark,
            depth,
            account_lifetime,
            rpc,
        }: ConfigWoChains,
        db: Database,
        chain_manager: oneshot::Receiver<ChainManager>,
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

        let recipient_ss58 = recipient.to_base58_string(2); //TODO

        let server_info = ServerInfo {
            // TODO
            version: env!("CARGO_PKG_VERSION"),
            instance_id: instance_id.clone(),
            debug,
            kalatori_remark: remark.clone(),
        };

        let state = StateData {
            currencies,
            recipient: recipient_ss58,
            server_info,
            db,
            seed_entropy,
        };

        // Remember to always spawn async here or things might deadlock
        task_tracker.spawn("State Handler", async move {
            let chain_manager = chain_manager.await.map_err(|_| Error::Fatal)?;
            while let Some(request) = rx.recv().await {
                match request {
                    StateAccessRequest::GetInvoiceStatus(request) => {
                        request
                            .res
                            .send(state.get_invoice_status(request.order).await).map_err(|_| Error::Fatal)?;
                    }
                    StateAccessRequest::CreateInvoice(request) => {
                        request
                            .res
                            .send(state.create_invoice(request.order_query).await).map_err(|_| Error::Fatal)?;
                    }
                    StateAccessRequest::ServerStatus(res) => {
                        let server_status = ServerStatus {
                            description: state.server_info.clone(),
                            supported_currencies: state.currencies.clone(),
                        };
                        res.send(server_status).map_err(|_| Error::Fatal)?;
                    }
                    // Orchestrate shutdown from here
                    StateAccessRequest::Shutdown => {
                        chain_manager.shutdown().await;
                        state.db.shutdown().await;
                        break;
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
            .await.map_err(|_| Error::Fatal)?;
        rx.await.map_err(|_| Error::Fatal)?
    }

    pub async fn server_status(&self) -> Result<ServerStatus, Error> {
        let (res, rx) = oneshot::channel();
        self.tx.send(StateAccessRequest::ServerStatus(res)).await.map_err(|_| Error::Fatal)?;
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
            .await.map_err(|_| Error::Fatal)?;
        rx.await.map_err(|_| Error::Fatal)?
    }

    pub fn interface(&self) -> Self {
        State {
            tx: self.tx.clone(),
        }
    }

    pub async fn shutdown(&self) {
        self.tx.send(StateAccessRequest::Shutdown).await.unwrap();
    }
}

enum StateAccessRequest {
    GetInvoiceStatus(GetInvoiceStatus),
    CreateInvoice(CreateInvoice),
    ServerStatus(oneshot::Sender<ServerStatus>),
    Shutdown,
}

struct GetInvoiceStatus {
    pub order: String,
    pub res: oneshot::Sender<Result<OrderResponse, Error>>,
}

struct CreateInvoice {
    pub order_query: OrderQuery,
    pub res: oneshot::Sender<Result<OrderResponse, Error>>,
}

struct StateData {
    currencies: HashMap<String, CurrencyProperties>,
    recipient: String,
    server_info: ServerInfo,
    db: Database,
    seed_entropy: Entropy,
}

impl StateData {
    async fn get_invoice_status(&self, order: String) -> Result<OrderResponse, Error> {
        if let Some(order_info) = self.db.read_order(order.clone()).await? {
            let message = String::new(); //TODO
            Ok(OrderResponse::FoundOrder(OrderStatus {
                order,
                message,
                recipient: self.recipient.clone(),
                server_info: self.server_info.clone(),
                order_info,
            }))
        } else {
            Ok(OrderResponse::NotFound)
        }
    }

    async fn create_invoice(&self, order_query: OrderQuery) -> Result<OrderResponse, Error> {
        let order = order_query.order.clone();
        let currency = self
            .currencies
            .get(&order_query.currency)
            .ok_or(ErrorOrder::UnknownCurrency)?;
        let currency = currency.info(order_query.currency.clone());
        let payment_account = Pair::from_entropy_and_full_derivation(
            &self.seed_entropy,
            derivations(&self.recipient, &order_query.order),
        )?
        .public()
        .to_base58_string(currency.ss58);
        let order_info = OrderInfo::new(order_query, currency, payment_account);
        match self
            .db
            .create_order(order.clone(), order_info.clone())
            .await?
        {
            OrderCreateResponse::New => Ok(OrderResponse::NewOrder(self.order_status(
                order,
                order_info,
                String::new(),
            ))),
            OrderCreateResponse::Modified => Ok(OrderResponse::ModifiedOrder(self.order_status(
                order,
                order_info,
                String::new(),
            ))),
            OrderCreateResponse::Collision(order_status) => {
                Ok(OrderResponse::CollidedOrder(self.order_status(
                    order,
                    order_info,
                    String::from("Order with this ID was already processed"),
                )))
            }
        }
    }

    fn order_status(&self, order: String, order_info: OrderInfo, message: String) -> OrderStatus {
        OrderStatus {
            order,
            message,
            recipient: self.recipient.clone(),
            server_info: self.server_info.clone(),
            order_info,
        }
    }
}
