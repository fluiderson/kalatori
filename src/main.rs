use clap::Parser;
use std::{borrow::Cow, future::Future, process::ExitCode, str, sync::Arc};
use substrate_crypto_light::common::{AccountId32, AsBase58};
use tokio::{
    runtime::Runtime,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot, RwLock,
    },
    task::JoinHandle,
};
use tokio_util::{sync::CancellationToken, task};
use tracing::Level;

mod callback;
mod chain;
mod database;
mod definitions;
mod error;
mod server;
mod signer;
mod state;
mod utils;

use arguments::{CliArgs, Config, SeedEnvVars, DATABASE_DEFAULT};
use chain::ChainManager;
use database::ConfigWoChains;
use error::{Error, PrettyCause};
use shutdown::{ShutdownNotification, ShutdownReason};
use signer::Signer;
use state::State;

fn main() -> ExitCode {
    let shutdown_notification = ShutdownNotification::new();

    // Sets the panic hook to print directly to the standard error because the logger isn't
    // initialized yet.
    shutdown::set_panic_hook(|panic| eprintln!("{panic}"), shutdown_notification.clone());

    match try_main(shutdown_notification) {
        Ok(failed) => match *failed.blocking_read() {
            ShutdownReason::UserRequested => {
                tracing::info!("Goodbye!");

                ExitCode::SUCCESS
            }
            ShutdownReason::UnrecoverableError => {
                tracing::error!("Badbye! The daemon's shut down with errors.");

                ExitCode::FAILURE
            }
        },
        Err(error) => {
            let print = |message| {
                if tracing::event_enabled!(Level::ERROR) {
                    tracing::error!("{message}");
                } else {
                    eprintln!("{message}");
                }
            };

            print(format_args!(
                "Badbye! The daemon's got a fatal error:\n    {error}.{}",
                error.pretty_cause()
            ));

            ExitCode::FAILURE
        }
    }
}

fn try_main(
    shutdown_notification: ShutdownNotification,
) -> Result<Arc<RwLock<ShutdownReason>>, Error> {
    let cli_args = CliArgs::parse();

    logger::initialize(cli_args.log)?;
    shutdown::set_panic_hook(
        |panic| tracing::error!("{panic}"),
        shutdown_notification.clone(),
    );

    let seed_env_vars = SeedEnvVars::parse()?;
    let config = Config::parse(cli_args.config)?;

    Runtime::new()
        .map_err(Error::Runtime)?
        .block_on(async_try_main(
            shutdown_notification,
            cli_args.recipient,
            cli_args.remark,
            config,
            seed_env_vars,
        ))
}

async fn async_try_main(
    shutdown_notification: ShutdownNotification,
    recipient_string: String,
    remark: Option<String>,
    config: Config,
    seed_env_vars: SeedEnvVars,
) -> Result<Arc<RwLock<ShutdownReason>>, Error> {
    let database_path = if config.in_memory_db {
        if config.database.is_some() {
            tracing::warn!(
                "`database` is set in the config but ignored because `in_memory_db` is \"true\""
            );
        }

        None
    } else {
        Some(config.database.unwrap_or_else(|| DATABASE_DEFAULT.into()))
    };

    let instance_id = String::from("TODO: add unique ID and save it in db");

    // Start services

    tracing::info!(
        "Kalatori {} by {} is starting on {}...",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS"),
        config.host,
    );

    let (task_tracker, error_rx) = TaskTracker::new();

    let recipient = AccountId32::from_base58_string(&recipient_string)
        .map_err(Error::RecipientAccount)?
        .0;

    let signer = Signer::init(recipient, task_tracker.clone(), seed_env_vars.seed)?;

    let db =
        database::Database::init(database_path, task_tracker.clone(), config.account_lifetime)?;

    let (cm_tx, cm_rx) = oneshot::channel();

    let state = State::initialise(
        signer.interface(),
        ConfigWoChains {
            recipient,
            debug: config.debug,
            remark,
            //depth: config.depth,
        },
        db,
        cm_rx,
        instance_id,
        task_tracker.clone(),
        shutdown_notification.token.clone(),
    )?;

    cm_tx
        .send(ChainManager::ignite(
            config.chain,
            state.interface(),
            signer.interface(),
            task_tracker.clone(),
            shutdown_notification.token.clone(),
        )?)
        .map_err(|_| Error::Fatal)?;

    let server = server::new(
        shutdown_notification.token.clone(),
        config.host,
        state.interface(),
    )
    .await?;

    task_tracker.spawn("the server module", server);

    let shutdown_completed = CancellationToken::new();
    let mut shutdown_listener = tokio::spawn(shutdown::listener(
        shutdown_notification.token.clone(),
        shutdown_completed.clone(),
    ));

    // Main loop

    Ok(tokio::select! {
        biased;
        reason = task_tracker.wait_with_notification(error_rx, shutdown_notification) => {
            shutdown_completed.cancel();
            shutdown_listener.await.expect("shutdown listener shouldn't panic")?;

            reason
        }
        error = &mut shutdown_listener => {
            return Err(
                error
                    .expect("shutdown listener shouldn't panic")
                    .expect_err("shutdown listener should only complete on errors here")
            );
        }
    })
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
        task: impl Future<Output = Result<Cow<'static, str>, Error>> + Send + 'static,
    ) -> JoinHandle<()> {
        let error_tx = self.error_tx.clone();

        self.inner.spawn(async move {
            match task.await {
                Ok(shutdown_message) => {
                    if !shutdown_message.is_empty() {
                        tracing::info!("{shutdown_message}");
                    }
                }
                Err(error) => error_tx.send((name.into(), error)).unwrap(),
            }
        })
    }

    async fn wait_with_notification(
        self,
        mut error_rx: UnboundedReceiver<(Cow<'static, str>, Error)>,
        shutdown_notification: ShutdownNotification,
    ) -> Arc<RwLock<ShutdownReason>> {
        // `self` holds the last `error_tx`, so we need to drop it; otherwise it'll create a
        // deadlock on `error_rx.recv()`.
        drop(self.error_tx);

        let mut failed = false;

        while let Some((from, error)) = error_rx.recv().await {
            tracing::error!(
                "Received a fatal error from {from}:\n    {error:?}.{}",
                error.pretty_cause()
            );

            if failed || !shutdown_notification.is_ignited() {
                tracing::info!("Initialising the shutdown...");

                failed = true;

                shutdown_notification.ignite().await;
            }
        }

        self.inner.wait().await;

        shutdown_notification.reason
    }

    /* async fn try_wait(
        self,
        mut error_rx: UnboundedReceiver<(Cow<'static, str>, Error)>,
    ) -> Result<(), Error> {
        // `self` holds the last `error_tx`, so we need to drop it; otherwise it'll create a
        // deadlock on `error_rx.recv()`.
        drop(self.error_tx);

        if let Some((from, error)) = error_rx.recv().await {
            return Err(error)?;
        }

        self.inner.wait().await;

        Ok(())
    } */
}

mod arguments {
    use crate::{
        definitions::{api_v2::Timestamp, Chain},
        error::SeedEnvError,
        logger, Error,
    };
    use ahash::AHashMap;
    use clap::{Arg, ArgAction, Parser};
    use serde::Deserialize;
    use std::{
        env, fs,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        str,
    };
    use toml_edit::de;

    shadow_rs::shadow!(shadow);

    use shadow::{BUILD_TIME_3339, RUST_VERSION, SHORT_COMMIT};

    pub const SEED: &str = "SEED";
    pub const OLD_SEED: &str = "OLD_SEED_";

    const SOCKET_DEFAULT: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 16726);
    pub const DATABASE_DEFAULT: &str = "kalatori.db";

    #[derive(Parser)]
    #[command(
        about,
        disable_help_flag(true),
        arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::Help)
                .help("Print this text.")
        ),
        after_help(concat!(
            "`SEED` is a required environment variable.\n\nMore documentation can be found at ",
            env!("CARGO_PKG_REPOSITORY"),
            ".\n\nCopyright (C) 2024 ",
            clap::crate_authors!()
        )),
        disable_version_flag(true),
        version,
        arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::Version)
                .help("Print the daemon version (and its build metadata).")
        ),
        long_version(shadow_rs::formatcp!(
            "{} ({SHORT_COMMIT})\n\nBuilt on {}\nwith {RUST_VERSION}.",
            clap::crate_version!(),
            // Replaces the local offset part with the "00:00" (aka "Z") UTC offset.
            shadow_rs::str_splice!(
                BUILD_TIME_3339,
                // TODO: Use `checked_sub()` with `expect()` instead.
                // https://github.com/rust-lang/rust/issues/67441
                BUILD_TIME_3339.len().saturating_sub(6)..,
                "Z"
            ).output,
        )),
    )]
    pub struct CliArgs {
        #[arg(
            short,
            long,
            env,
            value_name("PATH"),
            default_value("configs/polkadot.toml")
        )]
        pub config: String,

        #[arg(
            short,
            long,
            env,
            value_name("DIRECTIVES"),
            default_value(logger::default_filter()),
            default_missing_value(""),
            num_args(0..=1),
            require_equals(true),
        )]
        pub log: String,

        #[arg(long, env, visible_alias("rmrk"), value_name("STRING"))]
        pub remark: Option<String>,

        #[arg(short, long, env, value_name("HEX/SS58 ADDRESS"))]
        pub recipient: String,
    }

    pub struct SeedEnvVars {
        pub seed: String,
        pub old_seeds: AHashMap<String, String>,
    }

    impl SeedEnvVars {
        pub fn parse() -> Result<Self, SeedEnvError> {
            const SEED_BYTES: &[u8] = SEED.as_bytes();

            let mut seed_option = None;
            let mut old_seeds = AHashMap::new();

            for (raw_key, raw_value) in env::vars_os() {
                match raw_key.as_encoded_bytes() {
                    SEED_BYTES => {
                        env::remove_var(raw_key);

                        seed_option = {
                            Some(
                                raw_value
                                    .into_string()
                                    .map_err(|_| SeedEnvError::InvalidUnicodeValue(SEED.into()))?,
                            )
                        };
                    }
                    raw_key_bytes => {
                        // TODO: Use `OsStr::slice_encoded_bytes()` instead.
                        // https://github.com/rust-lang/rust/issues/118485
                        if let Some(stripped_raw_key) =
                            raw_key_bytes.strip_prefix(OLD_SEED.as_bytes())
                        {
                            env::remove_var(&raw_key);

                            let key = str::from_utf8(stripped_raw_key)
                                .map_err(|_| SeedEnvError::InvalidUnicodeOldSeedKey)?;
                            let value =
                                raw_value.to_str().ok_or(SeedEnvError::InvalidUnicodeValue(
                                    format!("{OLD_SEED}{key}").into(),
                                ))?;

                            old_seeds.insert(key.into(), value.into());
                        }
                    }
                }
            }

            Ok(Self {
                seed: seed_option.ok_or(SeedEnvError::SeedNotPresent)?,
                old_seeds,
            })
        }
    }

    /// User-supplied settings through the config file.
    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct Config {
        pub account_lifetime: Timestamp,
        #[serde(default = "default_host")]
        pub host: SocketAddr,
        pub database: Option<String>,
        pub debug: Option<bool>,
        #[serde(default)]
        pub in_memory_db: bool,
        pub chain: Vec<Chain>,
    }

    impl Config {
        pub fn parse(path: String) -> Result<Self, Error> {
            let unparsed_config =
                fs::read_to_string(&path).map_err(|e| Error::ConfigFileRead(path, e))?;

            de::from_str(&unparsed_config).map_err(Into::into)
        }
    }

    fn default_host() -> SocketAddr {
        SOCKET_DEFAULT
    }
}

mod logger {
    use crate::{callback, chain, database, server, Error};
    use tracing_subscriber::{fmt::time::UtcTime, EnvFilter};

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

    pub fn initialize(directives: String) -> Result<(), Error> {
        let filter =
            EnvFilter::try_new(&directives).map_err(|e| Error::LoggerDirectives(directives, e))?;

        tracing_subscriber::fmt()
            .with_timer(UtcTime::rfc_3339())
            .with_env_filter(filter)
            .init();

        Ok(())
    }

    fn default_filter_capacity() -> usize {
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
        )
    }

    pub fn default_filter() -> String {
        let mut filter = String::with_capacity(default_filter_capacity());

        filter.push_str(OFF);

        for target in TARGETS {
            filter.push_str(COMMA);
            filter.push_str(target);
            filter.push_str(INFO);
        }

        filter
    }

    #[cfg(test)]
    mod tests {
        use tracing_subscriber::EnvFilter;

        #[test]
        fn default_filter_capacity() {
            assert_eq!(
                super::default_filter().len(),
                super::default_filter_capacity()
            );
        }

        #[test]
        fn default_filter_is_valid() {
            assert!(EnvFilter::try_new(super::default_filter()).is_ok());
        }
    }
}

mod shutdown {
    use crate::Error;
    use std::{
        fmt::{Display, Formatter, Result as FmtResult},
        io::Result as IoResult,
        panic::{self, PanicInfo},
        process,
        sync::Arc,
        time::Duration,
    };
    use tokio::{signal, sync::RwLock, time};
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    #[allow(clippy::module_name_repetitions)]
    pub struct ShutdownNotification {
        pub token: CancellationToken,
        pub reason: Arc<RwLock<ShutdownReason>>,
    }

    impl ShutdownNotification {
        pub fn new() -> Self {
            Self {
                token: CancellationToken::new(),
                reason: Arc::new(RwLock::new(ShutdownReason::UserRequested)),
            }
        }

        pub fn is_ignited(&self) -> bool {
            self.token.is_cancelled()
        }

        pub async fn ignite(&self) {
            *self.reason.write().await = ShutdownReason::UnrecoverableError;
            self.token.cancel();
        }
    }

    pub async fn listener(
        shutdown_notification: CancellationToken,
        shutdown_completed: CancellationToken,
    ) -> Result<(), Error> {
        const TIP_TIMEOUT_SECS: u64 = 30;

        tokio::select! {
            biased;
            result = signal::ctrl_c() => {
                process_signal(result)?;

                tracing::info!("Received the shutdown signal. Initialising the shutdown...");

                shutdown_notification.cancel();
            }
            () = shutdown_notification.cancelled() => {}
        }

        let shutdown_completed_clone = shutdown_completed.clone();
        let tip = tokio::spawn(async move {
            tokio::select! {
                biased;
                () = shutdown_completed_clone.cancelled() => {}
                () = time::sleep(Duration::from_secs(TIP_TIMEOUT_SECS)) => {
                    tracing::warn!(
                        "Send the shutdown signal one more time to kill the daemon instead of waiting for the graceful shutdown."
                    );
                }
            }
        });

        tokio::select! {
            biased;
            () = shutdown_completed.cancelled() => {}
            result = signal::ctrl_c() => {
                process_signal(result)?;

                tracing::info!("Received the second shutdown signal. Killing the daemon...");

                // TODO: Use `ExitCode::exit_process()` instead.
                // https://github.com/rust-lang/rust/issues/97100
                process::abort()
            }
        }

        tip.await.expect("tip task shouldn't panic");

        tracing::info!("The shutdown signal listener is shut down.");

        Ok(())
    }

    fn process_signal(result: IoResult<()>) -> Result<(), Error> {
        result.map_err(Error::ShutdownSignal)?;

        // Print shutdown log messages on the next line after the Control-C command.
        println!();

        Ok(())
    }

    #[derive(Clone, Copy)]
    #[allow(clippy::module_name_repetitions)]
    pub enum ShutdownReason {
        UserRequested,
        UnrecoverableError,
    }

    pub fn set_panic_hook(
        print: impl Fn(PrettyPanic<'_>) + Send + Sync + 'static,
        shutdown_notification: ShutdownNotification,
    ) {
        panic::set_hook(Box::new(move |panic_info| {
            let reason = *shutdown_notification.reason.blocking_read();

            let first = match reason {
                ShutdownReason::UserRequested => false,
                ShutdownReason::UnrecoverableError => {
                    *shutdown_notification.reason.blocking_write() =
                        ShutdownReason::UnrecoverableError;

                    true
                }
            };

            print(PrettyPanic { panic_info, first });

            shutdown_notification.token.cancel();
        }));
    }

    pub struct PrettyPanic<'a> {
        panic_info: &'a PanicInfo<'a>,
        first: bool,
    }

    // It looks like it's impossible to acquire `PanicInfo` outside of `panic::set_hook`, which
    // could alter execution of other unit tests, so, without mocking the `panic_info` field,
    // there's no way to test the `Display`ing.
    impl Display for PrettyPanic<'_> {
        fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
            f.write_str("A panic detected")?;

            if let Some(location) = self.panic_info.location() {
                f.write_str(" at ")?;
                location.fmt(f)?;
            }

            let payload = self.panic_info.payload();
            let message_option = match payload.downcast_ref() {
                Some(string) => Some(*string),
                None => payload.downcast_ref::<String>().map(|string| &string[..]),
            };

            if let Some(panic_message) = message_option {
                f.write_str(":\n    ")?;
                f.write_str(panic_message)?;
            }

            f.write_str(".")?;

            // Print the report request only on the first panic.

            if self.first {
                f.write_str(concat!(
                    "\n\nThis is a bug. Please report it at ",
                    env!("CARGO_PKG_REPOSITORY"),
                    "/issues."
                ))?;
            }

            Ok(())
        }
    }
}
