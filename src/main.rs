use ahash::RandomState;
use anyhow::{Context, Error, Result};
use hex_simd::AsciiCase;
use serde::Deserialize;
use std::{
    borrow::Cow,
    collections,
    env::{self, VarError},
    error::Error as _,
    ffi::OsString,
    fmt::{self, Debug, Formatter},
    fs,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Deref,
    panic, str,
    sync::Arc,
};
use subxt::{
    config::{substrate::SubstrateHeader, PolkadotExtrinsicParams},
    ext::{
        futures::TryFutureExt,
        sp_core::{
            crypto::{AccountId32, Ss58Codec},
            paste,
            sr25519::Pair,
            Pair as _,
        },
    },
    PolkadotConfig,
};
use tokio::{
    signal,
    sync::mpsc::{self, error::SendError, UnboundedReceiver, UnboundedSender},
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
use database::{ChainHash, PublicSlot, State, StateParameters, StoredChainInfo};
use rpc::{InnerConnectedChain, Processor};

const CONFIG: &str = "KALATORI_CONFIG";
const LOG: &str = "KALATORI_LOG";
const SEED: &str = "KALATORI_SEED";
const RECIPIENT: &str = "KALATORI_RECIPIENT";
const REMARK: &str = "KALATORI_REMARK";
const OLD_SEED: &str = "KALATORI_OLD_SEED_";

const CONFIG_BYTES: &[u8] = CONFIG.as_bytes();
const SEED_BYTES: &[u8] = SEED.as_bytes();
const RECIPIENT_BYTES: &[u8] = RECIPIENT.as_bytes();
const REMARK_BYTES: &[u8] = REMARK.as_bytes();
const OLD_SEED_BYTES: &[u8] = OLD_SEED.as_bytes();

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
type HashMap<K, V> = collections::HashMap<K, V, RandomState>;

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

fn main() -> Result<()> {
    try_main().map_err(|error| {
        tracing::info!("Badbye!");

        error
    })
}

#[tokio::main]
async fn try_main() -> Result<()> {
    let shutdown_notification = CancellationToken::new();

    set_panic_hook(shutdown_notification.clone());
    initialize_logger()?;

    let env_vars = EnvVars::parse()?;

    if let Some(remark) = &env_vars.remark {
        tracing::info!("`{REMARK}`: {remark:?}.");
    } else {
        tracing::info!("`{REMARK}` isn't set.");
    }

    let (pair, old_pairs) = (
        env_vars
            .pair
            .with_context(|| format!("`{SEED}` isn't set"))?,
        env_vars.old_pairs,
    );
    let recipient = env_vars
        .recipient
        .with_context(|| format!("`{RECIPIENT}` isn't set"))?;

    let config = Config::parse(env_vars.config)?;

    if let Some(bugde) = config.debug {
        // For some reason, "`{debug}`" here results into a compilation error, don't change "bugde".
        tracing::info!("\"debug\": `{bugde}`.");
    } else {
        tracing::info!("\"debug\" isn't set.");
    }

    if let Some(invoices_sweep_trigger) = config.invoices_sweep_trigger {
        tracing::info!("\"invoices-sweep-trigger\": {invoices_sweep_trigger}.");
    } else {
        tracing::info!("\"invoices-sweep-trigger\" isn't set.");
    }

    let host = if let Some(unparsed_host) = &config.host {
        unparsed_host
            .parse()
            .context("failed to convert `host` from the config to a socket address")?
    } else {
        DEFAULT_SOCKET
    };

    let database_path = if config.in_memory_db.unwrap_or_default() {
        if config.database.is_some() {
            tracing::warn!(
                "`database` is set in the config but ignored because `in_memory_db` is \"true\""
            );
        }

        None
    } else {
        Some(config.database.map_or_else(|| {
            tracing::debug!(
                "`database` isn't present in the config, using the default value instead: {DEFAULT_DATABASE:?}."
            );

            DEFAULT_DATABASE.into()
        }, Into::into))
    };

    tracing::info!(
        "Kalatori {} by {} is starting...",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS")
    );

    let (chains, currencies) = rpc::prepare(
        config.chain.unwrap_or_else(|| {
            tracing::warn!(
                "The config contains no chains, the daemon will start in the idle mode."
            );

            vec![]
        }),
        config.account_lifetime,
    )
    .await
    .context("failed to prepare the RPC module")?;

    tracing::info!(
        "The number of served chains: {}. With the total number of currencies: {}.",
        chains.len(),
        currencies.len()
    );

    let (state, checked_chains) = State::initialize(StateParameters {
        path_option: database_path,
        debug: config.debug,
        recipient,
        remark: env_vars.remark,
        pair,
        old_pairs,
        account_lifetime: config.account_lifetime,
        chains,
        currencies,
        invoices_sweep_trigger: config.invoices_sweep_trigger,
    })
    .context("failed to initialize the database module")?;

    let (task_tracker, error_rx) = TaskTracker::new();

    // task_tracker.spawn(
    //     "the shutdown listener",
    //     shutdown_listener(shutdown_notification.clone()),
    // );

    for (db_chain_info, chain) in checked_chains {
        // task_tracker.spawn_chain_processor(state.clone(), db_chain_info, chain);
    }

    // task_tracker.spawn(
    //     "proc",
    //     Processor::ignite(state.clone(), shutdown_notification.clone()),
    // );

    // let server = server::new(shutdown_notification.clone(), host, state)
    //     .await
    //     .context("failed to initialise the server module")?;

    // task_tracker.spawn(shutdown(
    //     processor.ignite(last_saved_block, task_tracker.clone(), error_tx.clone()),
    //     error_tx,
    // ));
    // task_tracker.spawn("the server module", server);

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
            .map(|location| format!(" at {location}"))
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
    let filter = EnvFilter::try_from_env(LOG).or_else(|error| {
        if *error
            .source()
            .expect("should always be `Some`")
            .downcast_ref::<VarError>()
            .expect("`EnvFilter::try_from_env()` error should downcast to `VarError`")
            != VarError::NotPresent
        {
            return Err(error).with_context(|| format!("failed to parse `{LOG}`"));
        }

        let predefined = if cfg!(debug_assertions) {
            EnvFilter::try_new("debug")
        } else {
            EnvFilter::try_new(default_filter())
        }
        .expect("predefined log directives should be valid");

        Ok(predefined)
    })?;

    tracing_subscriber::fmt()
        .with_timer(UtcTime::rfc_3339())
        .with_env_filter(filter)
        .init();

    Ok(())
}

#[derive(Default)]
struct EnvVars {
    config: Option<String>,
    pair: Option<Pair>,
    recipient: Option<AccountId>,
    remark: Option<String>,
    old_pairs: HashMap<PublicSlot, (Pair, String)>,
}

impl EnvVars {
    fn parse() -> Result<Self> {
        let mut this = Self::default();

        for (raw_key, raw_value) in env::vars_os() {
            match raw_key.as_encoded_bytes() {
                CONFIG_BYTES => {
                    this.config = Some(os_string_to_string(raw_value, CONFIG)?);
                }
                SEED_BYTES => {
                    this.pair = Some(
                        Pair::from_string(&os_string_to_string(raw_value, SEED)?, None)
                            .with_context(|| format!("failed to parse `{SEED}`"))?,
                    );
                }
                RECIPIENT_BYTES => {
                    let recipient =
                        AccountId::from_string(&os_string_to_string(raw_value, RECIPIENT)?)
                            .with_context(|| format!("failed to parse `{RECIPIENT}`"))?;

                    tracing::info!("Recipient: {}.", encode_to_hex(&recipient));

                    this.recipient = Some(recipient);
                }
                REMARK_BYTES => {
                    this.remark = Some(os_string_to_string(raw_value, REMARK)?);
                }
                raw_key_bytes => {
                    if let Some(stripped_raw_key) = raw_key_bytes.strip_prefix(OLD_SEED_BYTES) {
                        let key = str::from_utf8(stripped_raw_key).with_context(|| {
                            format!("failed to read one of the `{OLD_SEED}*` variables")
                        })?;
                        let value = raw_value.to_str().with_context(|| {
                            format!("seed phrase from `{OLD_SEED}{key}` contains invalid Unicode")
                        })?;
                        let old_pair = Pair::from_string(value, None).with_context(|| {
                            format!("failed to generate a key pair from `{OLD_SEED}{key}`")
                        })?;

                        this.old_pairs
                            .insert(old_pair.public().0, (old_pair, key.to_owned()));
                    }
                }
            }
        }

        Ok(this)
    }
}

fn os_string_to_string(os_string: OsString, env_name: &str) -> Result<String> {
    os_string
        .into_string()
        .map_err(|_| anyhow::anyhow!("`{env_name}` contains invalid Unicode"))
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

struct TaskError {
    task: Cow<'static, str>,
    error: Error,
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
        self.spawn_inner(async {
            task.await.map_err(|error| TaskError {
                task: name.into(),
                error,
            })
        })
    }

    fn spawn_inner(
        &self,
        task: impl Future<Output = Result<Cow<'static, str>, TaskError>> + Send + 'static,
    ) -> JoinHandle<()> {
        let error_tx = self.error_tx.clone();

        self.inner.spawn(async move {
            match task.await {
                Ok(shutdown_message) if !shutdown_message.is_empty() => {
                    tracing::info!("{shutdown_message}");
                }
                Err(task_error) => {
                    if let Err(SendError((from, unsent_error))) =
                        error_tx.send((task_error.task, task_error.error))
                    {
                        tracing::error!("Received a fatal error from {from}:\n{unsent_error:?}");
                    }
                }
                _ => {}
            }
        })
    }

    fn spawn_chain_processor(
        &self,
        state: Arc<State>,
        db_chain_info: StoredChainInfo,
        chain: InnerConnectedChain,
    ) -> JoinHandle<()> {
        self.spawn_inner(async move {
            let hash = db_chain_info.hash;

            Processor::ignite(db_chain_info, chain, &state)
                .await
                .map_err(|error| TaskError {
                    task: format!("the {:?} chain processor", state.chain_name(hash)).into(),
                    error,
                })
        })
    }

    async fn wait_with_notification(
        self,
        mut error_rx: UnboundedReceiver<(Cow<'static, str>, Error)>,
        shutdown_notification: CancellationToken,
    ) {
        // `self` holds the last `error_tx`, so we need to drop it; otherwise it'll create a
        // deadlock at `error_rx.recv()`.
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

fn encode_to_hex(data: impl AsRef<[u8]>) -> String {
    const PREFIX: &str = "0x";

    let data_bytes = data.as_ref();
    let mut string = String::with_capacity(PREFIX.len().saturating_add(data_bytes.len()));

    string.push_str(PREFIX);

    hex_simd::encode_append(data, &mut string, AsciiCase::Lower);

    string
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Config {
    account_lifetime: Timestamp,
    host: Option<String>,
    database: Option<String>,
    debug: Option<bool>,
    in_memory_db: Option<bool>,
    chain: Option<Vec<Chain>>,
    invoices_sweep_trigger: Option<Timestamp>,
}

impl Config {
    fn parse(path_option: Option<String>) -> Result<Self> {
        let (config, path): (_, Cow<'_, _>) = if let Some(path) = path_option {
            (fs::read_to_string(&path), path.into())
        } else {
            tracing::debug!("`{CONFIG}` isn't present, using the default value instead.");

            (fs::read_to_string(DEFAULT_CONFIG), DEFAULT_CONFIG.into())
        };

        tracing::info!("`{CONFIG}`: {path}.");

        de::from_str(&config.with_context(|| format!("failed to read a config file at {path:?}"))?)
            .with_context(|| format!("failed to parse the config at {path:?}"))
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

struct Balance(u128);

impl Debug for Balance {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

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
