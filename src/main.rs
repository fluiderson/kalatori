use anyhow::{Context, Error, Result};
use env_logger::{Builder, Env};
use log::LevelFilter;
use serde::Deserialize;
use std::{
    collections::HashMap,
    env::{self, VarError},
    fs,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Deref,
    panic, str,
};
use subxt::{
    config::PolkadotExtrinsicParams,
    ext::{
        codec::{Encode, Output},
        scale_decode::DecodeAsType,
        scale_encode::{self, EncodeAsType, TypeResolver},
        sp_core::{
            crypto::{AccountId32, Ss58Codec},
            sr25519::Pair,
            Pair as _,
        },
    },
    PolkadotConfig,
};
use tokio::{
    signal,
    sync::mpsc::{self, UnboundedSender},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use toml_edit::de;

mod callback;
mod database;
mod rpc;
mod server;

const CONFIG: &str = "KALATORI_CONFIG";
const LOG: &str = "KALATORI_LOG";
const LOG_STYLE: &str = "KALATORI_LOG_STYLE";
const SEED: &str = "KALATORI_SEED";
const OLD_SEED: &str = "KALATORI_OLD_SEED_";

const DB_VERSION: Version = 0;

const DEFAULT_CONFIG: &str = "kalatori.toml";
const DEFAULT_SOCKET: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 16726);
const DEFAULT_DATABASE: &str = "kalatori.xlsx";

type AssetId = u32;
type Decimals = u8;
type BlockNumber = u64;
type ExtrinsicIndex = u32;
type Version = u64;
type AccountId = <RuntimeConfig as subxt::Config>::AccountId;
type Nonce = u32;
type Timestamp = u64;
type PalletIndex = u8;
type BlockHash = <RuntimeConfig as subxt::Config>::Hash;
type OnlineClient = subxt::OnlineClient<RuntimeConfig>;

struct RuntimeConfig;

impl subxt::Config for RuntimeConfig {
    type Hash = <PolkadotConfig as subxt::Config>::Hash;
    type AccountId = AccountId32;
    type Address = <PolkadotConfig as subxt::Config>::Address;
    type Signature = <PolkadotConfig as subxt::Config>::Signature;
    type Hasher = <PolkadotConfig as subxt::Config>::Hasher;
    type Header = <PolkadotConfig as subxt::Config>::Header;
    type ExtrinsicParams = PolkadotExtrinsicParams<Self>;
    type AssetId = u32;
}

enum Asset {
    Id(AssetId),
    MultiLocation(PalletIndex, AssetId),
}

impl EncodeAsType for Asset {
    fn encode_as_type_to<R: TypeResolver>(
        &self,
        type_id: &R::TypeId,
        types: &R,
        out: &mut Vec<u8>,
    ) -> Result<(), scale_encode::Error> {
        match self {
            Self::Id(id) => id.encode_as_type_to(type_id, types, out),
            Self::MultiLocation(assets_pallet, asset_id) => {
                MultiLocation::new(*assets_pallet, *asset_id).encode_as_type_to(type_id, types, out)
            }
        }
    }
}

impl Encode for Asset {
    fn size_hint(&self) -> usize {
        match self {
            Self::Id(id) => id.size_hint(),
            Self::MultiLocation(assets_pallet, asset_id) => {
                MultiLocation::new(*assets_pallet, *asset_id).size_hint()
            }
        }
    }

    fn encode_to<T: Output + ?Sized>(&self, dest: &mut T) {
        match self {
            Self::Id(id) => id.encode_to(dest),
            Self::MultiLocation(assets_pallet, asset_id) => {
                MultiLocation::new(*assets_pallet, *asset_id).encode_to(dest);
            }
        }
    }
}

#[derive(EncodeAsType, DecodeAsType, Encode)]
#[encode_as_type(crate_path = "subxt::ext::scale_encode")]
#[decode_as_type(crate_path = "subxt::ext::scale_decode")]
#[codec(crate = subxt::ext::codec)]
struct MultiLocation {
    parents: u8,
    interior: Junctions,
}

impl MultiLocation {
    fn new(assets_pallet: PalletIndex, asset_id: AssetId) -> Self {
        Self {
            parents: 0,
            interior: Junctions::X2(
                Junction::PalletInstance(assets_pallet),
                Junction::GeneralIndex(asset_id.into()),
            ),
        }
    }
}

#[derive(EncodeAsType, DecodeAsType, Encode)]
#[encode_as_type(crate_path = "subxt::ext::scale_encode")]
#[decode_as_type(crate_path = "subxt::ext::scale_decode")]
#[codec(crate = subxt::ext::codec)]
enum Junctions {
    #[codec(index = 2)]
    X2(Junction, Junction),
}

#[derive(EncodeAsType, DecodeAsType, Encode)]
#[encode_as_type(crate_path = "subxt::ext::scale_encode")]
#[decode_as_type(crate_path = "subxt::ext::scale_decode")]
#[codec(crate = subxt::ext::codec)]
pub enum Junction {
    #[codec(index = 4)]
    PalletInstance(PalletIndex),
    #[codec(index = 5)]
    GeneralIndex(u128),
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    let mut builder = Builder::new();

    if cfg!(debug_assertions) {
        builder.filter_level(LevelFilter::Debug)
    } else {
        builder
            .filter_level(LevelFilter::Off)
            .filter_module(callback::MODULE, LevelFilter::Info)
            .filter_module(database::MODULE, LevelFilter::Info)
            .filter_module(rpc::MODULE, LevelFilter::Info)
            .filter_module(server::MODULE, LevelFilter::Info)
            .filter_module(env!("CARGO_PKG_NAME"), LevelFilter::Info)
    }
    .parse_env(Env::from(LOG).write_style(LOG_STYLE))
    .init();

    let pair = Pair::from_string(
        &env::var(SEED).with_context(|| format!("failed to read `{SEED}`"))?,
        None,
    )
    .with_context(|| format!("failed to generate a key pair from `{SEED}`"))?;
    let pair_public = pair.public();

    let mut old_seeds = HashMap::new();

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
            let old_pair_public = old_pair.public();

            if old_pair_public == pair_public {
                anyhow::bail!("public key generated from `{OLD_SEED}{key}` equals the one generated from `{SEED}`");
            }

            old_seeds.insert(key.to_owned(), (old_pair, old_pair_public));
        }
    }

    let config_path = env::var(CONFIG).or_else(|error| match error {
        VarError::NotUnicode(_) => Err(error).with_context(|| format!("failed to read `{CONFIG}`")),
        VarError::NotPresent => {
            log::debug!(
                "`{CONFIG}` isn't present, using the default value instead: {DEFAULT_CONFIG:?}."
            );

            Ok(DEFAULT_CONFIG.into())
        }
    })?;
    let unparsed_config = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read a config file at {config_path:?}"))?;
    let config: Config = de::from_str(&unparsed_config)
        .with_context(|| format!("failed to parse the config at {config_path:?}"))?;

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
                break 'database None;
            }
        } else if config.in_memory_db.is_some() {
            log::warn!("`in_memory_db` is set in the config but ignored because `debug` isn't set");
        }

        Some(config.database.unwrap_or_else(|| {
            log::debug!(
                "`database` isn't present in the config, using the default value instead: {DEFAULT_DATABASE:?}."
            );

            DEFAULT_DATABASE.to_owned()
        }))
    };

    let recipient = AccountId::from_string(&config.recipient)
        .context("failed to convert `recipient` from the config to an account address")?;

    log::info!(
        "Kalatori {} by {} is starting...",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS")
    );

    let shutdown_notification = CancellationToken::new();
    let (error_tx, mut error_rx) = mpsc::unbounded_channel();
    let shutdown_notification_for_panic = shutdown_notification.clone();

    panic::set_hook(Box::new(move |panic_info| {
        let at = panic_info
            .location()
            .map(|location| format!(" at `{location}`"))
            .unwrap_or_default();
        let panic_message = panic_info
            .payload()
            .downcast_ref::<&str>()
            .map_or_else(|| ".".into(), |message| format!(":\n{message}\n"));

        log::error!(
            "A panic detected{at}{panic_message}\nThis is a bug. Please report it at {}.",
            env!("CARGO_PKG_REPOSITORY")
        );

        shutdown_notification_for_panic.cancel();
    }));

    let chains = rpc::prepare(config.chain)
        .await
        .context("failed while preparing the RPC module")?;

    // let (database, last_saved_block) = Database::initialise(
    //     database_path,
    //     override_rpc,
    //     pair,
    //     endpoint_properties,
    //     destination,
    // )
    // .context("failed to initialise the database module")?;

    // let processor = Processor::new(api_config, database.clone(), shutdown_notification.clone())
    //     .context("failed to initialise the RPC module")?;

    let server = server::new(shutdown_notification.clone(), host)
        .await
        .context("failed to initialise the server module")?;

    let task_tracker = TaskTracker::new();

    task_tracker.close();
    task_tracker.spawn(try_task(
        "the shutdown listener",
        shutdown_listener(shutdown_notification.clone()),
        error_tx.clone(),
    ));
    // task_tracker.spawn(shutdown(
    //     processor.ignite(last_saved_block, task_tracker.clone(), error_tx.clone()),
    //     error_tx,
    // ));
    task_tracker.spawn(try_task("the server module", server, error_tx));

    while let Some((from, error)) = error_rx.recv().await {
        log::error!("Received a fatal error from {from}!\n{error:?}");

        if !shutdown_notification.is_cancelled() {
            log::info!("Initialising the shutdown...");

            shutdown_notification.cancel();
        }
    }

    task_tracker.wait().await;

    log::info!("Goodbye!");

    Ok(())
}

async fn try_task<'a>(
    name: &'a str,
    task: impl Future<Output = Result<String>>,
    error_tx: UnboundedSender<(&'a str, Error)>,
) {
    match task.await {
        Ok(shutdown_message) if !shutdown_message.is_empty() => log::info!("{shutdown_message}"),
        Err(error) => error_tx
            .send((name, error))
            .expect("error channel shouldn't be dropped/closed"),
        _ => {}
    }
}

async fn shutdown_listener(shutdown_notification: CancellationToken) -> Result<String> {
    tokio::select! {
        biased;
        signal = signal::ctrl_c() => {
            signal.context("failed to listen for the shutdown signal")?;

            // Print shutdown log messages on the next line after the Control-C command.
            println!();

            log::info!("Received the shutdown signal. Initialising the shutdown...");

            shutdown_notification.cancel();
        }
        () = shutdown_notification.cancelled() => {}
    }

    Ok("The shutdown signal listener is shut down.".into())
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Config {
    recipient: String,
    account_lifetime: Timestamp,
    depth: Timestamp,
    host: Option<String>,
    database: Option<String>,
    remark: Option<String>,
    debug: Option<bool>,
    in_memory_db: Option<bool>,
    chain: Vec<Chain>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Chain {
    name: String,
    endpoints: Vec<String>,
    #[serde(flatten)]
    native_token: Option<NativeToken>,
    asset: Option<Vec<AssetInfo>>,
    multi_location_assets: Option<bool>,
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

#[derive(Debug)]
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
