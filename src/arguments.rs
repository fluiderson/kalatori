use crate::{
    chain::definitions::Decimals,
    database::definitions::{Asset, BlockNumber, Timestamp},
    logger, Error,
};
use clap::{Arg, ArgAction, Parser};
use serde::Deserialize;
use std::{
    borrow::Cow,
    env, fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str,
    sync::Arc,
};
use toml_edit::de;

shadow_rs::shadow!(shadow);

use shadow::{BUILD_TIME_3339, RUST_VERSION, SHORT_COMMIT};

macro_rules! env_var_prefix {
    ($var:literal) => {
        concat!("KALATORI_", $var)
    };
}

pub const SEED: &str = env_var_prefix!("SEED");
pub const OLD_SEED: &str = env_var_prefix!("OLD_SEED_");

const YEAR: &str = "2024";

macro_rules! default_var {
    ($var:ident: $var_type:ty = $var_value:expr) => {
        #[allow(non_snake_case)]
        fn $var() -> $var_type {
            const VAR: $var_type = $var_value;

            VAR
        }
    };
}

default_var!(DATABASE: Cow<'static, str> = Cow::Borrowed("kalatori.db"));
default_var!(HOST: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 16726));

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
    after_help(const_format::concatcp!(
        const_format::formatcp!("`{SEED}`"),
        " is a required environment variable.\n\nMore documentation can be found at ",
        env!("CARGO_PKG_REPOSITORY"),
        const_format::formatcp!(".\n\nCopyright (C) {YEAR} "),
        clap::crate_authors!()
    )),
    disable_version_flag(true),
    arg(
        Arg::new("version")
            .short('V')
            .long("version")
            .action(ArgAction::Version)
            .help("Print the daemon version (and its build metadata).")
    ),
    long_version(const_format::concatcp!(
        clap::crate_version!(),
        const_format::formatcp!(" ({SHORT_COMMIT})\n\nBuilt on "),
        // Replaces the local offset part (e.g. "-08:00") with the "00:00" (aka "Z") UTC offset.
        const_format::str_splice!(BUILD_TIME_3339, BUILD_TIME_3339.len() - 6.., "Z").output,
        const_format::formatcp!("\nwith {RUST_VERSION}.")
    )),
)]
pub struct CliArgs {
    #[arg(
        short,
        long,
        env(env_var_prefix!("CONFIG")),
        value_name("PATH"),
        default_value("configs/polkadot.toml")
    )]
    pub config: String,

    #[arg(
        short,
        long,
        env(env_var_prefix!("LOG")),
        value_name("DIRECTIVES"),
        default_value(logger::default_filter()),
        default_missing_value("debug"),
        num_args(0..=1),
        require_equals(true),
    )]
    pub log: String,

    #[arg(long, env(env_var_prefix!("REMARK")), visible_alias("rmrk"), value_name("STRING"))]
    pub remark: Option<String>,

    #[arg(short, long, env(env_var_prefix!("RECIPIENT")), value_name("HEX/SS58 ADDRESS"))]
    pub recipient: String,

    #[arg(
        short,
        long,
        env(env_var_prefix!("DATABASE")),
        value_name("PATH"),
    )]
    #[allow(clippy::option_option)]
    pub database: Option<Option<String>>,
}

/// User-supplied settings through the config file.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    #[serde(flatten)]
    pub intervals: ChainIntervals,
    #[serde(default = "HOST")]
    pub host: SocketAddr,
    #[serde(default = "DATABASE")]
    pub database: Cow<'static, str>,
    #[serde(default)]
    pub debug: bool,
    #[serde(default)]
    pub chain: Vec<Chain>,
}

impl Config {
    pub fn parse(path: String) -> Result<Self, Error> {
        let unparsed_config =
            fs::read_to_string(&path).map_err(|e| Error::ConfigFileRead(path, e))?;

        de::from_str(&unparsed_config).map_err(Into::into)
    }
}

#[derive(Deserialize, Clone)]
pub struct Chain {
    pub name: Arc<str>,
    #[serde(flatten)]
    pub config: ChainConfig,
}

#[derive(Deserialize, Clone)]
pub struct ChainConfig {
    pub endpoints: Vec<String>,
    #[serde(flatten)]
    pub inner: ChainConfigInner,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ChainConfigInner {
    pub native: Option<Native>,
    #[serde(default)]
    pub asset: Vec<AssetInfo>,
    #[serde(flatten)]
    pub intervals: ChainIntervals,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct ChainIntervals {
    pub account_lifetime: Option<Timestamp>,
    pub account_lifetime_in_blocks: Option<BlockNumber>,
    pub restart_gap: Option<Timestamp>,
    pub restart_gap_in_blocks: Option<BlockNumber>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Native {
    pub name: String,
    pub decimals: Decimals,
}

#[derive(Deserialize, Clone, Debug)]
pub struct AssetInfo {
    pub name: String,
    pub id: Asset,
}
