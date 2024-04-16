use anyhow::{Context, Error, Result};
use serde::Deserialize;
use std::{
    borrow::Cow,
    collections::HashMap,
    env::{self, VarError},
    error::Error as _,
    fs,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Deref,
    panic, str,
};
use subxt::{
    config::{substrate::SubstrateHeader, PolkadotExtrinsicParams},
    ext::sp_core::{
        crypto::{AccountId32, Ss58Codec},
        sr25519::{Pair, Public},
        Pair as _,
    },
    PolkadotConfig,
};
use tokio::{
    signal,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tokio_util::{sync::CancellationToken, task};
use toml_edit::de;
use tracing_subscriber::{fmt::time::UtcTime, EnvFilter};

mod asset;
mod callback;
mod database;
mod rpc;
mod server;

use asset::Asset;
use database::{ConfigWoChains, State};
use rpc::Processor;

const CONFIG: &str = "KALATORI_CONFIG";
const LOG: &str = "KALATORI_LOG";
const SEED: &str = "KALATORI_SEED";
const RECIPIENT: &str = "KALATORI_RECIPIENT";
const REMARK: &str = "KALATORI_REMARK";
const OLD_SEED: &str = "KALATORI_OLD_SEED_";

const DB_VERSION: Version = 0;

const DEFAULT_CONFIG: &str = "configs/polkadot.toml";
const DEFAULT_SOCKET: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 16726);
const DEFAULT_DATABASE: &str = "kalatori.db";

type AssetId = u32;
type Decimals = u8;
type BlockNumber = u64;
type ExtrinsicIndex = u32;
type Version = u64;
type Nonce = u32;
type Timestamp = u64;
type PalletIndex = u8;

type BlockHash = <RuntimeConfig as subxt::Config>::Hash;
type AccountId = <RuntimeConfig as subxt::Config>::AccountId;
type OnlineClient = subxt::OnlineClient<RuntimeConfig>;

struct RuntimeConfig;

impl subxt::Config for RuntimeConfig {
    type Hash = <PolkadotConfig as subxt::Config>::Hash;
    type AccountId = AccountId32;
    type Address = <PolkadotConfig as subxt::Config>::Address;
    type Signature = <PolkadotConfig as subxt::Config>::Signature;
    type Hasher = <PolkadotConfig as subxt::Config>::Hasher;
    type Header = SubstrateHeader<BlockNumber, Self::Hasher>;
    type ExtrinsicParams = PolkadotExtrinsicParams<Self>;
    type AssetId = Asset;
}

#[tokio::main]
async fn main() -> Result<()> {
    let shutdown_notification = CancellationToken::new();

    set_panic_hook(shutdown_notification.clone());
    initialize_logger()?;

    let (pair, old_pairs) = parse_seeds()?;
    let recipient = env::var(RECIPIENT).with_context(|| format!("failed to read {RECIPIENT}"))?;

    let remark = match env::var(REMARK) {
        Ok(remark) => Some(remark),
        Err(VarError::NotPresent) => None,
        Err(error) => Err(error).with_context(|| format!("failed to read {REMARK}"))?,
    };

    let config = Config::parse()?;

    let host = if let Some(unparsed_host) = config.host {
        unparsed_host
            .parse()
            .context("failed to convert `host` from the config to a socket address")?
    } else {
        DEFAULT_SOCKET
    };

    let debug = config.debug.unwrap_or_default();

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

    tracing::info!(
        "Kalatori {} by {} is starting on {}...",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS"),
        host,
    );

    let (task_tracker, error_rx) = TaskTracker::new();

    task_tracker.spawn(
        "the shutdown listener",
        shutdown_listener(shutdown_notification.clone()),
    );

    let (chains, currencies) = rpc::prepare(config.chain, config.account_lifetime, config.depth)
        .await
        .context("failed while preparing the RPC module")?;

    let rpc = env::var("KALATORI_RPC").unwrap();

    let state = State::initialise(
        database_path,
        currencies,
        pair,
        old_pairs,
        ConfigWoChains {
            recipient,
            debug: config.debug,
            remark,
            depth: config.depth,
            account_lifetime: config.account_lifetime,
            rpc,
        },
    )
    .context("failed to initialise the database module")?;

    task_tracker.spawn(
        "proc",
        Processor::ignite(state.clone(), shutdown_notification.clone()),
    );

    let server = server::new(shutdown_notification.clone(), host, state)
        .await
        .context("failed to initialise the server module")?;

    // task_tracker.spawn(shutdown(
    //     processor.ignite(last_saved_block, task_tracker.clone(), error_tx.clone()),
    //     error_tx,
    // ));
    task_tracker.spawn("the server module", server);

    task_tracker
        .wait_with_notification(error_rx, shutdown_notification)
        .await;

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
            "A panic detected{at}{formatted_message}\nThis is a bug. Please report it at {}.",
            env!("CARGO_PKG_REPOSITORY")
        );

        shutdown_notification.cancel();
    }));
}

fn initialize_logger() -> Result<()> {
    let filter = match EnvFilter::try_from_env(LOG) {
        Err(error) => {
            let Some(VarError::NotPresent) = error
                .source()
                .expect("should always be `Some`")
                .downcast_ref()
            else {
                return Err(error).with_context(|| format!("failed to parse `{LOG}`"));
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
        rpc::MODULE,
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

fn parse_seeds() -> Result<(Pair, HashMap<String, Pair>)> {
    let pair = Pair::from_string(
        &env::var(SEED).with_context(|| format!("failed to read `{SEED}`"))?,
        None,
    )
    .with_context(|| format!("failed to generate a key pair from `{SEED}`"))?;

    let mut old_pairs = HashMap::new();

    for (raw_key, raw_value) in env::vars_os() {
        let raw_key_bytes = raw_key.as_encoded_bytes();

        if let Some(stripped_raw_key) = raw_key_bytes.strip_prefix(OLD_SEED.as_bytes()) {
            let key = str::from_utf8(stripped_raw_key)
                .context("failed to read an old seed environment variable name")?;
            let value = raw_value
                .to_str()
                .with_context(|| format!("failed to read a seed phrase from `{OLD_SEED}{key}`"))?;
            let old_pair = Pair::from_string(value, None)
                .with_context(|| format!("failed to generate a key pair from `{OLD_SEED}{key}`"))?;

            old_pairs.insert(key.to_owned(), old_pair);
        }
    }

    Ok((pair, old_pairs))
}

#[derive(Clone)]
struct TaskTracker {
    inner: task::TaskTracker,
    error_tx: UnboundedSender<(Cow<'static, str>, Error)>,
}

impl TaskTracker {
    fn new() -> (Self, UnboundedReceiver<(Cow<'static, str>, Error)>) {
        let (error_tx, error_rx) = mpsc::unbounded_channel();
        let inner = task::TaskTracker::new();

        inner.close();

        (Self { inner, error_tx }, error_rx)
    }

    fn spawn(
        &self,
        name: impl Into<Cow<'static, str>> + Send + 'static,
        task: impl Future<Output = Result<Cow<'static, str>>> + Send + 'static,
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
        mut error_rx: UnboundedReceiver<(Cow<'static, str>, Error)>,
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
        mut error_rx: UnboundedReceiver<(Cow<'static, str>, Error)>,
    ) -> Result<()> {
        drop(self.error_tx);

        if let Some((from, error)) = error_rx.recv().await {
            return Err(error).with_context(|| format!("received a fatal error from {from}"));
        }

        self.inner.wait().await;

        Ok(())
    }
}

async fn shutdown_listener(shutdown_notification: CancellationToken) -> Result<Cow<'static, str>> {
    tokio::select! {
        biased;
        signal = signal::ctrl_c() => {
            signal.context("failed to listen for the shutdown signal")?;

            // Print shutdown log messages on the next line after the Control-C command.
            println!();

            tracing::info!("Received the shutdown signal. Initialising the shutdown...");

            shutdown_notification.cancel();
        }
        () = shutdown_notification.cancelled() => {}
    }

    Ok("The shutdown signal listener is shut down.".into())
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Config {
    account_lifetime: Timestamp,
    depth: Option<Timestamp>,
    host: Option<String>,
    database: Option<String>,
    debug: Option<bool>,
    in_memory_db: Option<bool>,
    chain: Vec<Chain>,
}

impl Config {
    fn parse() -> Result<Self> {
        let config_path = env::var(CONFIG).or_else(|error| match error {
            VarError::NotUnicode(_) => {
                Err(error).with_context(|| format!("failed to read `{CONFIG}`"))
            }
            VarError::NotPresent => {
                tracing::debug!(
                    "`{CONFIG}` isn't present, using the default value instead: {DEFAULT_CONFIG:?}."
                );

                Ok(DEFAULT_CONFIG.into())
            }
        })?;
        let unparsed_config = fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read a config file at {config_path:?}"))?;

        de::from_str(&unparsed_config)
            .with_context(|| format!("failed to parse the config at {config_path:?}"))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Chain {
    name: String,
    endpoints: Vec<String>,
    #[serde(flatten)]
    native_token: Option<NativeToken>,
    asset: Option<Vec<AssetInfo>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct NativeToken {
    native_token: String,
    decimals: Decimals,
}

#[derive(Deserialize)]
struct AssetInfo {
    name: String,
    id: AssetId,
}

#[derive(Deserialize, Debug, Clone, Copy)]
struct Balance(u128);

impl Deref for Balance {
    type Target = u128;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Balance {
    fn format(&self, decimals: Decimals) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let float = **self as f64;

        float / decimal_exponent_product(decimals)
    }

    fn parse(float: f64, decimals: Decimals) -> Self {
        let parsed_float = (float * decimal_exponent_product(decimals)).round();

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self(parsed_float as _)
    }
}

fn decimal_exponent_product(decimals: Decimals) -> f64 {
    10f64.powi(decimals.into())
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
