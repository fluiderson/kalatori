use clap::Parser;
use std::{process::ExitCode, sync::Arc};
use substrate_crypto_light::common::{AccountId32, AsBase58};
use tokio::{
    runtime::Runtime,
    sync::{oneshot, RwLock},
};
use tokio_util::sync::CancellationToken;
use tracing::Level;

use kalatori::arguments::{CliArgs, Config, SeedEnvVars, DATABASE_DEFAULT};
use kalatori::chain::ChainManager;
use kalatori::database::ConfigWoChains;
use kalatori::error::{Error, PrettyCause};
use kalatori::logger;
use kalatori::shutdown::{set_panic_hook, ShutdownNotification, ShutdownReason};
use kalatori::signer::Signer;
use kalatori::state::State;
use kalatori::task_tracker::TaskTracker;

fn main() -> ExitCode {
    let shutdown_notification = ShutdownNotification::new();

    // Sets the panic hook to print directly to the standard error because the logger isn't
    // initialized yet.
    set_panic_hook(|panic| eprintln!("{panic}"), shutdown_notification.clone());

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
) -> Result<Arc<tokio::sync::RwLock<ShutdownReason>>, Error> {
    let cli_args = CliArgs::parse();

    logger::initialize(cli_args.log)?;
    set_panic_hook(
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

    let db = kalatori::database::Database::init(
        database_path,
        task_tracker.clone(),
        config.account_lifetime,
    )?;

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

    let server = kalatori::server::new(
        shutdown_notification.token.clone(),
        config.host,
        state.interface(),
    )
    .await?;

    task_tracker.spawn("the server module", server);

    let shutdown_completed = CancellationToken::new();
    let mut shutdown_listener = tokio::spawn(kalatori::shutdown::listener(
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
