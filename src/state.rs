use crate::{
    chain::ChainManager,
    database::Database,
    definitions::{
        api_v2::{
            CurrencyProperties, OrderCreateResponse, OrderInfo, OrderQuery, OrderResponse,
            OrderStatus, ServerInfo, ServerStatus,
        },
        Entropy,
    },
    error::{Error, OrderError},
    signer::Signer,
    ConfigWoChains, TaskTracker,
};

use std::collections::HashMap;

use substrate_crypto_light::common::{AccountId32, AsBase58};
use tokio::sync::oneshot;

/// Struct to store state of daemon. If something requires cooperation of more than one component,
/// it should go through here.
#[derive(Clone, Debug)]
pub struct State {
    tx: tokio::sync::mpsc::Sender<StateAccessRequest>,
}

impl State {
    pub fn initialise(
        signer: Signer,
        ConfigWoChains {
            recipient,
            debug,
            remark,
            account_lifetime,
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

        let server_info = ServerInfo {
            // TODO
            version: env!("CARGO_PKG_VERSION").to_string(),
            instance_id: instance_id.clone(),
            debug,
            kalatori_remark: remark.clone(),
        };

        // Remember to always spawn async here or things might deadlock
        task_tracker.clone().spawn("State Handler", async move {
            let chain_manager = chain_manager.await.map_err(|_| Error::Fatal)?;
            let db_wakeup = db.clone();
            let chain_manager_wakeup = chain_manager.clone();
            let currencies = HashMap::new();
            let mut state = StateData {
                currencies,
                recipient,
                server_info,
                db,
                chain_manager,
                signer,
            };

            // TODO: consider doing this even more lazy
            let order_list = db_wakeup.order_list().await?;
            task_tracker.spawn("Restore saved orders", async move {
                for (order, order_details) in order_list {
                    chain_manager_wakeup
                        .add_invoice(order, order_details, state.recipient)
                        .await;
                }
                Ok("All saved orders restored".into())
            });

            while let Some(request) = rx.recv().await {
                match request {
                    StateAccessRequest::ConnectChain(assets) => {
                        // it MUST be asserted in chain tracker that assets are those and only
                        // those that user requested
                        state.update_currencies(assets);
                    }
                    StateAccessRequest::GetInvoiceStatus(request) => {
                        request
                            .res
                            .send(state.get_invoice_status(request.order).await)
                            .map_err(|_| Error::Fatal)?;
                    }
                    StateAccessRequest::CreateInvoice(request) => {
                        request
                            .res
                            .send(state.create_invoice(request.order_query).await)
                            .map_err(|_| Error::Fatal)?;
                    }
                    StateAccessRequest::ServerStatus(res) => {
                        let server_status = ServerStatus {
                            server_info: state.server_info.clone(),
                            supported_currencies: state.currencies.clone(),
                        };
                        res.send(server_status).map_err(|_| Error::Fatal)?;
                    }
                    StateAccessRequest::OrderPaid(id) => {
                        // Only perform actions if the record is saved in ledger
                        match state.db.mark_paid(id.clone()).await {
                            Ok(order) => {
                                // TODO: callback here
                                drop(state.chain_manager.reap(id, order, state.recipient).await);
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Order was paid but this could not be recorded! {e:?}"
                                )
                            }
                        }
                    }
                    // Orchestrate shutdown from here
                    StateAccessRequest::Shutdown => {
                        // Web server shuts down on its own; it does not matter what it sends now.

                        // First shut down active actions for external world. If something yet
                        // happens, we should record it in db.
                        state.chain_manager.shutdown().await;

                        // Now that nothing happens we can wind down the ledger
                        state.db.shutdown().await;

                        // Try to zeroize secrets
                        state.signer.shutdown().await;

                        // And shut down finally
                        break;
                    }
                };
            }

            Ok("State handler is shutting down".into())
        });

        Ok(Self { tx })
    }

    pub async fn connect_chain(&self, assets: HashMap<String, CurrencyProperties>) {
        self.tx.send(StateAccessRequest::ConnectChain(assets)).await;
    }

    pub async fn order_status(&self, order: &str) -> Result<OrderResponse, Error> {
        let (res, rx) = oneshot::channel();
        self.tx
            .send(StateAccessRequest::GetInvoiceStatus(GetInvoiceStatus {
                order: order.to_string(),
                res,
            }))
            .await
            .map_err(|_| Error::Fatal)?;
        rx.await.map_err(|_| Error::Fatal)?
    }

    pub async fn server_status(&self) -> Result<ServerStatus, Error> {
        let (res, rx) = oneshot::channel();
        self.tx
            .send(StateAccessRequest::ServerStatus(res))
            .await
            .map_err(|_| Error::Fatal)?;
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
            .await
            .map_err(|_| Error::Fatal)?;
        rx.await.map_err(|_| Error::Fatal)?
    }

    pub async fn order_paid(&self, order: String) {
        if self
            .tx
            .send(StateAccessRequest::OrderPaid(order))
            .await
            .is_err()
        {
            tracing::warn!("Data race on shutdown; please restart the daemon for cleaning up");
        };
    }

    pub async fn force_withdrawal(&self, order: String) -> Result<OrderStatus, OrderStatus> {
        todo!()
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
    ConnectChain(HashMap<String, CurrencyProperties>),
    GetInvoiceStatus(GetInvoiceStatus),
    CreateInvoice(CreateInvoice),
    ServerStatus(oneshot::Sender<ServerStatus>),
    OrderPaid(String),
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
    recipient: AccountId32,
    server_info: ServerInfo,
    db: Database,
    chain_manager: ChainManager,
    signer: Signer,
}

impl StateData {
    fn update_currencies(&mut self, currencies: HashMap<String, CurrencyProperties>) {
        self.currencies.extend(currencies);
    }

    async fn get_invoice_status(&self, order: String) -> Result<OrderResponse, Error> {
        if let Some(order_info) = self.db.read_order(order.clone()).await? {
            let message = String::new(); //TODO
            Ok(OrderResponse::FoundOrder(OrderStatus {
                order,
                message,
                recipient: self.recipient.clone().to_base58_string(2), // TODO maybe but spec says use "2"
                server_info: self.server_info.clone(),
                order_info,
                payment_page: String::new(),
                redirect_url: String::new(),
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
            .ok_or(OrderError::UnknownCurrency)?;
        let currency = currency.info(order_query.currency.clone());
        let payment_account = self.signer.public(order.clone(), currency.ss58).await?;
        let order_info = OrderInfo::new(order_query, currency, payment_account);
        match self
            .db
            .create_order(order.clone(), order_info.clone())
            .await?
        {
            OrderCreateResponse::New => {
                self.chain_manager
                    .add_invoice(order.clone(), order_info.clone(), self.recipient)
                    .await?;
                Ok(OrderResponse::NewOrder(self.order_status(
                    order,
                    order_info,
                    String::new(),
                )))
            }
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
            recipient: self.recipient.clone().to_base58_string(2), // TODO maybe but spec says use "2"
            server_info: self.server_info.clone(),
            order_info,
            payment_page: String::new(),
            redirect_url: String::new(),
        }
    }
}
