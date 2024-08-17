use chain2::ChainManager;
use clap::Parser;
use std::process::ExitCode;
use substrate_crypto_light::common::AccountId32;
use tokio::{runtime::Runtime, sync::oneshot};
use tokio_util::sync::CancellationToken;
use tracing::Level;

mod arguments;
mod callback;
mod chain;
mod chain2;
mod database;
// mod database2;
mod definitions;
mod error;
mod server;
mod signer;
// mod signer2;
mod state;
mod utils;

use arguments::{CliArgs, Config};
use chain::definitions::Account;
use database::Database;
// use database2::ConfigWoChains;
use error::{Error, PrettyCause};
use signer::KeyStore;
// use signer2::Signer;
use state::State;
use utils::{
    logger,
    shutdown::{self, ShutdownNotification, ShutdownOutcome},
    task_tracker::TaskTracker,
};

fn main() -> ExitCode {
    let shutdown_notification = ShutdownNotification::new();

    // Sets the panic hook to print directly to the standard error because the logger isn't
    // initialized yet.
    shutdown::set_panic_hook(|panic| eprintln!("{panic}"), shutdown_notification.clone());

    if let Err(error) = try_main(shutdown_notification.clone()) {
        // TODO: https://github.com/rust-lang/rust/issues/92698
        // An equilibristic to conditionally print an error message without storing it as
        // `String` on the heap.
        let print = |message| {
            if tracing::event_enabled!(Level::ERROR) {
                tracing::error!("{message}");
            } else {
                eprintln!("{message}");
            }
        };

        print(format_args!(
            "Badbye! The daemon's got an error during the initialization:{}",
            error.pretty_cause()
        ));

        ExitCode::FAILURE
    } else {
        match *shutdown_notification.outcome.blocking_read() {
            ShutdownOutcome::UserRequested => {
                tracing::info!("Goodbye!");

                ExitCode::SUCCESS
            }
            ShutdownOutcome::UnrecoverableError { panic } => {
                tracing::error!(
                    "Badbye! The daemon's shut down with errors{}.",
                    if panic { " due to internal bugs" } else { "" }
                );

                ExitCode::FAILURE
            }
        }
    }
}

fn try_main(shutdown_notification: ShutdownNotification) -> Result<(), Error> {
    let cli_args = CliArgs::parse();

    logger::initialize(cli_args.log)?;
    shutdown::set_panic_hook(
        |panic| tracing::error!("{panic}"),
        shutdown_notification.clone(),
    );

    tracing::info!("Kalatori {} is starting...", env!("CARGO_PKG_VERSION"));

    let recipient_account: Account = cli_args.recipient.parse()?;

    tracing::info!("The given recipient: {recipient_account:#}.");

    let recipient = recipient_account.into();
    let key_store = KeyStore::parse()?;
    let config = Config::parse(cli_args.config)?;

    Runtime::new()
        .map_err(Error::Runtime)?
        .block_on(async_try_main(
            shutdown_notification,
            recipient,
            cli_args.remark,
            cli_args.database,
            config,
            key_store,
        ))
}

#[allow(clippy::option_option)]
async fn async_try_main(
    shutdown_notification: ShutdownNotification,
    recipient: AccountId32,
    remark: Option<String>,
    db_option_option: Option<Option<String>>,
    config: Config,
    key_store: KeyStore,
) -> Result<(), Error> {
    let (task_tracker, error_rx) = TaskTracker::new();
    let connected_chains = chain::connect(config.chain.clone()).await?;
    let (database, signer) = Database::new(
        db_option_option.map_or(Some(config.database), |path| path.map(Into::into)),
        &connected_chains,
        key_store,
    )?;
    // let bababa = ChainManager::new(database.clone(), connected_chains, config.intervals).await?;

    let (cm_tx, cm_rx) = oneshot::channel();
    let state = State::initialise(
        signer.clone(),
        Some(config.debug),
        remark,
        database.clone(),
        cm_rx,
        task_tracker.clone(),
        shutdown_notification.token.clone(),
        recipient,
        config.intervals.account_lifetime.unwrap(),
    );

    cm_tx
        .send(ChainManager::ignite(
            config.chain,
            state.interface(),
            signer.clone(),
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

    tracing::info!("The initialization has been completed.");

    // Start the main loop and wait for it to gracefully end or the early termination signal.
    tokio::select! {
        biased;
        () = task_tracker.wait_and_shutdown(error_rx, shutdown_notification) => {
            shutdown_completed.cancel();

            shutdown_listener.await
        }
        shutdown_listener_result = &mut shutdown_listener => shutdown_listener_result
    }
    .expect("shutdown listener shouldn't panic")
}
