use anyhow::{Context, Result};
use database::{State, StateConfig};
use env_logger::{Builder, Env};
use environment_variables::*;
use log::LevelFilter;
use rpc::Processor;
use std::{
    env::{self, VarError},
    error::Error,
    str::FromStr,
    sync::Arc, fmt::{Display, Formatter, self},
};
use subxt::{
    backend::rpc::RpcClient,
    config::{DefaultExtrinsicParams, Header},
    ext::sp_core::{
        crypto::{AccountId32, Ss58Codec},
        Pair,
    },
    Config, PolkadotConfig,
};
use tokio::{
    signal,
    sync::watch::{Receiver, Sender},
    task::JoinSet,
};

mod database;
mod rpc;

pub mod server;

pub mod environment_variables {
    pub const HOST: &str = "KALATORI_HOST";
    pub const SEED: &str = "KALATORI_SEED";
    pub const LOG: &str = "KALATORI_LOG";
    pub const LOG_STYLE: &str = "KALATORI_LOG_STYLE";
    pub const DATABASE: &str = "KALATORI_DATABASE";
    pub const RPC: &str = "KALATORI_RPC";
    pub const IN_MEMORY_DB: &str = "KALATORI_IN_MEMORY_DB";
    pub const DECIMALS: &str = "KALATORI_DECIMALS";
    pub const RECIPIENT: &str = "KALATORI_RECIPIENT";
    pub const ASSET: &str = "KALATORI_ASSET";
    pub const FEE: &str = "KALATORI_FEE";
    pub const MAX_SUBSCRIPTIONS: &str = "KALATORI_MAX_SUBSCRIPTIONS";
    pub const DEPTH: &str = "KALATORI_DEPTH";
    pub const NO_UTILITY: &str = "KALATORI_NO_UTILITY";
}

pub const DEFAULT_RPC: &str = "wss://westend-asset-hub-rpc.polkadot.io";
pub const DEFAULT_DATABASE: &str = "database.redb";
pub const DATABASE_VERSION: Version = 1;
pub const MIN_SUBSCRIPTIONS: u32 = 3;

// https://github.com/paritytech/jsonrpsee/blob/72939a92312b65352fbb76b588e3aff6cb45615b/client/ws-client/src/lib.rs#L107
const SCANNER_TO_LISTENER_SWITCH_POINT: BlockNumber = 512;

type OnlineClient = subxt::OnlineClient<RuntimeConfig>;
type Account = <RuntimeConfig as Config>::AccountId;
type BlockNumber = <<RuntimeConfig as Config>::Header as Header>::Number;
type Asset = <RuntimeConfig as Config>::AssetId;
type Hash = <RuntimeConfig as Config>::Hash;
// https://github.com/paritytech/polkadot-sdk/blob/2fe3145ab9bd26ceb5a26baf2a64105b0035a5a6/substrate/bin/node/primitives/src/lib.rs#L43
type Balance = u128;
// https://github.com/paritytech/subxt/blob/0ea9c7ede6e8312bf3de0da06124361e5c2120c5/subxt/src/config/signed_extensions.rs#L43
type Nonce = u64;
// https://github.com/dtolnay/semver/blob/f9cc2df9415c880bd3610c2cdb6785ac7cad31ea/src/lib.rs#L163-L165
type Version = u64;
// https://github.com/paritytech/polkadot-sdk/blob/2fe3145ab9bd26ceb5a26baf2a64105b0035a5a6/substrate/frame/assets/src/types.rs#L198
type Decimals = u8;
// https://github.com/paritytech/polkadot-sdk/blob/2fe3145ab9bd26ceb5a26baf2a64105b0035a5a6/substrate/frame/utility/src/lib.rs#L132
type BatchedCallsLimit = u32;

struct RuntimeConfig;

impl Config for RuntimeConfig {
    type Hash = <PolkadotConfig as Config>::Hash;
    type AccountId = AccountId32;
    type Address = <PolkadotConfig as Config>::Address;
    type Signature = <PolkadotConfig as Config>::Signature;
    type Hasher = <PolkadotConfig as Config>::Hasher;
    type Header = <PolkadotConfig as Config>::Header;
    type ExtrinsicParams = DefaultExtrinsicParams<Self>;
    type AssetId = <PolkadotConfig as Config>::AssetId;
}

fn read_and_parse_env_var<T: FromStr<Err = impl Error + Send + Sync + 'static>>(
    key: &str,
    to: &str,
) -> Result<T> {
    env::var(key)
        .with_context(|| format!("failed to read `{key}`"))?
        .parse()
        .with_context(|| format!("failed to convert `{key}` to {to}"))
}

#[doc(hidden)]
#[tokio::main]
pub async fn main() -> Result<()> {
    let mut builder = Builder::new();

    if cfg!(debug_assertions) {
        builder.filter_level(LevelFilter::Debug)
    } else {
        builder
            .filter_level(LevelFilter::Off)
            .filter_module(server::MODULE, LevelFilter::Info)
            .filter_module(rpc::MODULE, LevelFilter::Info)
            .filter_module(database::MODULE, LevelFilter::Info)
            .filter_module(env!("CARGO_PKG_NAME"), LevelFilter::Info)
    }
    .parse_env(Env::from(LOG).write_style(LOG_STYLE))
    .init();

    // let host = read_and_parse_env_var(HOST, "a socket address")?;

    let pair = Pair::from_string(
        &env::var(SEED).with_context(|| format!("failed to read `{SEED}`"))?,
        None,
    )
    .with_context(|| format!("failed to generate a key pair from `{SEED}`"))?;

    let rpc_url = env::var(RPC).or_else(|error| match error {
        VarError::NotUnicode(_) => Err(error).context(format!("failed to read `{RPC}`")),
        VarError::NotPresent => {
            log::debug!(
                "`{RPC}` isn't present, using the default value instead: \"{DEFAULT_RPC}\"."
            );

            Ok(DEFAULT_RPC.into())
        }
    })?;

    let database_path = if env::var_os(IN_MEMORY_DB).is_none() {
        Some(env::var(DATABASE).or_else(|error| match error {
            VarError::NotUnicode(_) => Err(error).context(format!("failed to read `{DATABASE}`")),
            VarError::NotPresent => {
                log::debug!(
                    "`{DATABASE}` isn't present, using the default value instead: \"{DEFAULT_DATABASE}\"."
                );

                Ok(DEFAULT_DATABASE.into())
            }
        })?)
    } else {
        if env::var_os(DATABASE).is_some() {
            log::warn!(
                "`{IN_MEMORY_DB}` is set along with `{DATABASE}`. The latter will be ignored."
            );
        }

        None
    };

    let no_utility = env::var_os(NO_UTILITY).is_some();

    let asset = match env::var(ASSET) {
        Ok(asset) => asset
            .parse()
            .map(Some)
            .with_context(|| format!("failed to convert `{asset}` to an asset identifier")),
        Err(VarError::NotPresent) => Ok(None),
        Err(error @ VarError::NotUnicode(_)) => {
            Err(error).context(format!("failed to read `{ASSET}`"))
        }
    }?;

    let decimals = read_and_parse_env_var(DECIMALS, "the decimals places number type")?;

    let recipient = env::var(RECIPIENT)
        .with_context(|| format!("failed to read `{RECIPIENT}`"))
        .and_then(|recipient| {
            Account::from_string(&recipient)
                .with_context(|| format!("failed to convert `{RECIPIENT}` to an account address"))
        })?;

    let fee = read_and_parse_env_var(FEE, "the balance type")?;

    // let subscriptions = read_and_parse_env_var(MAX_SUBSCRIPTIONS, "a number")?;

    // if subscriptions < MIN_SUBSCRIPTIONS {
    //     anyhow::bail!("`{MAX_SUBSCRIPTIONS}` must be equal to or greater than {MIN_SUBSCRIPTIONS}");
    // }

    let depth = read_and_parse_env_var(DEPTH, "a block number")?;

    if depth == 0 {
        anyhow::bail!("`{DEPTH}` mustn't equal 0");
    }

    log::info!(
        "Kalatori {} by {} is starting...",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS")
    );

    let shutdown_notification = Arc::new(Sender::new(false));

    let (api_config, endpoint_properties, updater, finalized_block) = rpc::prepare(
        rpc_url,
        asset,
        no_utility,
        shutdown_notification.subscribe(),
    )
    .await
    .context("failed to prepare the RPC module")?;

    let (state, last_saved_block) = State::initialise(
        database_path,
        endpoint_properties,
        StateConfig {
            pair,
            recipient,
            asset,
            fee,
            decimals,
        },
        finalized_block.saturating_sub(depth),
    )
    .context("failed to initialise the daemon state")?;

    // let processor = Processor::new(
    //     api_config,
    //     database.clone(),
    //     shutdown_notification.subscribe(),
    // )
    // .context("failed to initialise the RPC module")?;

    // let server = server::new(shutdown_notification.subscribe(), host, database)
    //     .await
    //     .context("failed to initialise the server module")?;

    // let mut join_set = JoinSet::new();

    // join_set.spawn(shutdown_listener(
    //     shutdown_notification.clone(),
    //     shutdown_notification.subscribe(),
    // ));
    // join_set.spawn(updater.ignite());
    // join_set.spawn(processor.ignite(last_saved_block));
    // join_set.spawn(server);

    // while let Some(task) = join_set.join_next().await {
    //     let result = task.context("failed to shutdown a loop")?;

    //     match result {
    //         Ok(shutdown_message) => log::info!("{shutdown_message}"),
    //         Err(error) => {
    //             log::error!("Received a fatal error!\n{error:?}");

    //             if !*shutdown_notification.borrow() {
    //                 log::info!("Initialising the shutdown...");

    //                 shutdown_notification
    //                     .send(true)
    //                     .with_context(|| unexpected_closure_of_notification_channel("shutdown"))?;
    //             }
    //         }
    //     }
    // }

    log::info!("Goodbye!");

    Ok(())
}

async fn shutdown_listener(
    shutdown_notification_sender: Arc<Sender<bool>>,
    mut shutdown_notification_receiver: Receiver<bool>,
) -> Result<&'static str> {
    tokio::select! {
        biased;
        signal = signal::ctrl_c() => {
            signal.context("failed to listen for the shutdown signal")?;

            // Print shutdown log messages on the next line after the Control-C command.
            println!();

            log::info!("Received the shutdown signal. Initialising the shutdown...");

            process_shutdown_notification(shutdown_notification_sender.send(true), "send")
        }
        notification = shutdown_notification_receiver.changed() => {
            process_shutdown_notification(notification, "receive")
        }
    }
}

fn process_shutdown_notification<E>(
    result: impl Context<(), E>,
    kind: &str,
) -> Result<&'static str> {
    result
        .with_context(|| {
            unexpected_closure_of_notification_channel(&format!("shutdown listener ({kind})"))
        })
        .map(|()| "The shutdown signal listener is shut down.")
}

fn unexpected_closure_of_notification_channel(loop_name: &str) -> String {
    format!("unexpected closed shutdown notification channel in the {loop_name} loop")
}
