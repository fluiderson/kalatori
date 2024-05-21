use serde::Deserialize;
use std::{
    borrow::Cow,
    collections::HashMap,
    env::{self, VarError},
    error::Error as _,
    fs,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    panic, str,
    time::Duration,
};
use substrate_crypto_light::common::{AccountId32, AsBase58};
use tokio::{
    signal,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::{sync::CancellationToken, task};
use tracing_subscriber::{fmt::time::UtcTime, EnvFilter};

mod callback;
mod chain;
mod database;
mod definitions;
mod error;
mod server;
mod signer;
mod state;
mod utils;

use crate::definitions::{Chain, Entropy, Version};
use chain::ChainManager;
use database::ConfigWoChains;
use error::Error;
use signer::Signer;
use state::State;

const CONFIG: &str = "KALATORI_CONFIG";
const LOG: &str = "KALATORI_LOG";
const RECIPIENT: &str = "KALATORI_RECIPIENT";
const REMARK: &str = "KALATORI_REMARK";
const OLD_SEED: &str = "KALATORI_OLD_SEED_";

const DB_VERSION: Version = 0;

const DEFAULT_CONFIG: &str = "configs/polkadot.toml";
const DEFAULT_SOCKET: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 16726);
const DEFAULT_DATABASE: &str = "kalatori.db";

#[tokio::main]
async fn main() -> Result<(), Error> {
    let shutdown_notification = CancellationToken::new();

    set_panic_hook(shutdown_notification.clone());
    initialize_logger()?;

    // Read env

    let recipient = env::var(RECIPIENT).map_err(|_| Error::Env(RECIPIENT.to_string()))?;

    let remark = env::var(REMARK).map_err(|_| Error::Env(REMARK.to_string()))?;

    let config = Config::load()?;

    let host = if let Some(unparsed_host) = config.host {
        unparsed_host
            .parse()
            .map_err(|_| Error::ConfigParse("host to define a socket address".to_string()))?
    } else {
        DEFAULT_SOCKET
    };

    let debug = config.debug;

    let database_path = 'database: {
        if debug {
            if config.in_memory_db.unwrap_or_default() {
                if config.database.is_some() {
                    tracing::warn!(
                        "`database` is set in the config but ignored because `in_memory_db` is \"true\""
                    );
                }

                break 'database None;
            }
        } else if config.in_memory_db.is_some() {
            tracing::warn!(
                "`in_memory_db` is set in the config but ignored because `debug` isn't set"
            );
        }

        Some(config.database.unwrap_or_else(|| {
            tracing::debug!(
                "`database` isn't present in the config, using the default value instead: {DEFAULT_DATABASE:?}."
            );

            DEFAULT_DATABASE.into()
        }))
    };

    let instance_id = String::from("TODO: add unique ID and save it in db");

    // Start services

    tracing::info!(
        "Kalatori {} by {} is starting on {}...",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS"),
        host,
    );

    let (task_tracker, error_rx) = TaskTracker::new();

    let rpc = env::var("KALATORI_RPC").unwrap();

    let recipient = AccountId32::from_base58_string(&recipient)
        .map_err(Error::RecipientAccount)?
        .0;

    let signer = Signer::init(recipient.clone(), task_tracker.clone())?;

    let db = database::Database::init(database_path, task_tracker.clone())?;

    let (cm_tx, cm_rx) = oneshot::channel();

    let state = State::initialise(
        signer.interface(),
        ConfigWoChains {
            recipient: recipient.clone(),
            debug: config.debug,
            remark,
            //depth: config.depth,
            account_lifetime: Duration::from_millis(config.account_lifetime),
            rpc: rpc.clone(),
        },
        db,
        cm_rx,
        instance_id,
        task_tracker.clone(),
    )?;

    task_tracker.spawn(
        "the shutdown listener",
        shutdown_listener(shutdown_notification.clone(), state.interface()),
    );

    /*
    task_tracker.spawn(
        "proc",
        Processor::ignite(
            rpc,
            recipient.into(),
            state.clone(),
            shutdown_notification.clone(),
        ),
    );*/

    cm_tx
        .send(ChainManager::ignite(
            config.chain,
            state.interface(),
            signer.interface(),
            task_tracker.clone(),
            shutdown_notification.clone(),
        )?)
        .map_err(|_| Error::Fatal)?;

    let server = server::new(shutdown_notification.clone(), host, state.interface()).await?;

    // task_tracker.spawn(shutdown(
    //     processor.ignite(last_saved_block, task_tracker.clone(), error_tx.clone()),
    //     error_tx,
    // ));
    task_tracker.spawn("the server module", server);

    // Main loop

    task_tracker
        .wait_with_notification(error_rx, shutdown_notification)
        .await;

    // Shutdown

    tracing::info!("Goodbye!");

    Ok(())
}

fn set_panic_hook(shutdown_notification: CancellationToken) {
    panic::set_hook(Box::new(move |panic_info| {
        let at = panic_info
            .location()
            .map(|location| format!(" at `{location}`"))
            .unwrap_or_default();
        let payload = panic_info.payload();

        let message = match payload.downcast_ref::<&str>() {
            Some(string) => Some(*string),
            None => payload.downcast_ref::<String>().map(|string| &string[..]),
        };
        let formatted_message = match message {
            Some(string) => format!(":\n{string}\n"),
            None => ".".into(),
        };

        tracing::error!(
            "A panic detected{at}{formatted_message}\nThis is a bug. Please report it at {}/issues.",
            env!("CARGO_PKG_REPOSITORY")
        );

        shutdown_notification.cancel();
    }));
}

fn initialize_logger() -> Result<(), Error> {
    let filter = match EnvFilter::try_from_env(LOG) {
        Err(error) => {
            let Some(VarError::NotPresent) = error
                .source()
                .expect("should always be `Some`")
                .downcast_ref()
            else {
                return Err(Error::Env(LOG.to_string()));
            };

            if cfg!(debug_assertions) {
                EnvFilter::try_new("debug")
            } else {
                EnvFilter::try_new(default_filter())
            }
            .unwrap()
        }
        Ok(filter) => filter,
    };

    tracing_subscriber::fmt()
        .with_timer(UtcTime::rfc_3339())
        .with_env_filter(filter)
        .init();

    Ok(())
}

fn default_filter() -> String {
    const TARGETS: &[&str] = &[
        callback::MODULE,
        database::MODULE,
        chain::MODULE,
        server::MODULE,
        env!("CARGO_PKG_NAME"),
    ];
    const COMMA: &str = ",";
    const INFO: &str = "=info";
    const OFF: &str = "off";

    let mut filter = String::with_capacity(
        OFF.len().saturating_add(
            TARGETS
                .iter()
                .map(|module| {
                    COMMA
                        .len()
                        .saturating_add(module.len())
                        .saturating_add(INFO.len())
                })
                .sum(),
        ),
    );

    filter.push_str(OFF);

    for target in TARGETS {
        filter.push_str(COMMA);
        filter.push_str(target);
        filter.push_str(INFO);
    }

    filter
}

#[derive(Clone)]
struct TaskTracker {
    inner: task::TaskTracker,
    error_tx: mpsc::UnboundedSender<(Cow<'static, str>, Error)>,
}

impl TaskTracker {
    fn new() -> (Self, mpsc::UnboundedReceiver<(Cow<'static, str>, Error)>) {
        let (error_tx, error_rx) = mpsc::unbounded_channel();
        let inner = task::TaskTracker::new();

        inner.close();

        (Self { inner, error_tx }, error_rx)
    }

    fn spawn(
        &self,
        name: impl Into<Cow<'static, str>> + Send + 'static,
        task: impl Future<Output = Result<Cow<'static, str>, Error>> + Send + 'static,
    ) -> JoinHandle<()> {
        let error_tx = self.error_tx.clone();

        self.inner.spawn(async move {
            match task.await {
                Ok(shutdown_message) if !shutdown_message.is_empty() => {
                    tracing::info!("{shutdown_message}");
                }
                Err(error) => error_tx.send((name.into(), error)).unwrap(),
                _ => {}
            }
        })
    }

    async fn wait_with_notification(
        self,
        mut error_rx: mpsc::UnboundedReceiver<(Cow<'static, str>, Error)>,
        shutdown_notification: CancellationToken,
    ) {
        drop(self.error_tx);

        while let Some((from, error)) = error_rx.recv().await {
            tracing::error!("Received a fatal error from {from}:\n{error:?}");

            if !shutdown_notification.is_cancelled() {
                tracing::info!("Initialising the shutdown...");

                shutdown_notification.cancel();
            }
        }

        self.inner.wait().await;
    }

    async fn try_wait(
        self,
        mut error_rx: mpsc::UnboundedReceiver<(Cow<'static, str>, Error)>,
    ) -> Result<(), Error> {
        drop(self.error_tx);

        if let Some((from, error)) = error_rx.recv().await {
            return Err(error)?;
        }

        self.inner.wait().await;

        Ok(())
    }
}

async fn shutdown_listener(
    shutdown_notification: CancellationToken,
    state: State,
) -> Result<Cow<'static, str>, Error> {
    tokio::select! {
        biased;
        signal = signal::ctrl_c() => {
            signal.map_err(|_| Error::ShutdownSignal)?;

            // Print shutdown log messages on the next line after the Control-C command.
            println!();

            tracing::info!("Received the shutdown signal. Initialising the shutdown...");

            shutdown_notification.cancel();
            state.shutdown().await;
        }
        () = shutdown_notification.cancelled() => {}
    }

    Ok("The shutdown signal listener is shut down.".into())
}

/// User-supplied settings through config file
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Config {
    account_lifetime: u64,
    depth: Option<u64>,
    host: Option<String>,
    database: Option<String>,
    debug: bool,
    in_memory_db: Option<bool>,
    chain: Vec<Chain>,
}

impl Config {
    fn load() -> Result<Self, Error> {
        let config_path = env::var(CONFIG).or_else(|error| match error {
            VarError::NotUnicode(_) => Err(Error::Env(CONFIG.to_string())),
            VarError::NotPresent => {
                tracing::debug!(
                    "`{CONFIG}` isn't present, using the default value instead: {DEFAULT_CONFIG:?}."
                );

                Ok(DEFAULT_CONFIG.into())
            }
        })?;
        let unparsed_config = fs::read_to_string(&config_path)
            .map_err(|_| Error::ConfigFileRead(config_path.clone()))?;

        toml::from_str(&unparsed_config).map_err(Error::ConfigFileParse)
    }
}
