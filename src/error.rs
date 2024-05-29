use crate::definitions::api_v2::OrderStatus;
use frame_metadata::v15::RuntimeMetadataV15;
use jsonrpsee::core::ClientError;
use mnemonic_external::error::ErrorWordList;
use parity_scale_codec::Error as ScaleError;
use serde_json::Value;
use sled::Error as DatabaseError;
use std::net::SocketAddr;
use std::{
    error::Error as StdError,
    fmt::{Display, Formatter, Result},
    io::Error as IoError,
};
use substrate_constructor::error::{ErrorFixMe, StorageRegistryError};
use substrate_crypto_light::error::Error as CryptoError;
use substrate_parser::error::{MetaVersionErrorPallets, ParserError, RegistryError, StorageError};
use thiserror::Error;
use tokio::task::JoinError;
use toml::de::Error as TomlError;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to read the {0:?} environment variable")]
    Env(String),

    #[error("failed to read the config file at {0:?}")]
    ConfigFileRead(String),

    #[error("failed to parse the config")]
    ConfigFileParse(#[from] TomlError),

    #[error("failed to parse the config parameter {0:?}")]
    ConfigParse(String),

    #[error("chain {0:?} doesn't have any `endpoints` in the config")]
    EmptyEndpoints(String),

    #[error("RPC server error is occurred")]
    Chain(#[from] ChainError),

    #[error("database error is occurred")]
    Db(#[from] DbError),

    #[error("order error is occurred")]
    Order(#[from] OrderError),

    #[error("daemon server error is occurred")]
    Server(#[from] ServerError),

    #[error("signer error is occurred")]
    Signer(#[from] SignerError),

    #[error("failed to listen for the shutdown signal")]
    ShutdownSignal,

    #[error("receiver account couldn't be parsed")]
    RecipientAccount(#[from] CryptoError),

    #[error("fatal error is occurred")]
    Fatal,

    #[error("operating system related I/O error is occurred")]
    Io(#[from] IoError),

    #[error("found duplicate config record for the token {0:?}")]
    DuplicateCurrency(String),
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum ChainError {
    // TODO: this should be prevented by typesafety
    #[error("asset ID is missing")]
    AssetId,

    #[error("asset ID isn't `u32`")]
    AssetIdFormat,

    #[error("invalid assets for the chain {0:?}")]
    AssetsInvalid(String),

    #[error("asset key has no parceable part")]
    AssetKeyEmpty,

    #[error("asset key isn't single hash")]
    AssetKeyNotSingleHash,

    #[error("asset metadata isn't a map")]
    AssetMetadataPlain,

    #[error("unexpected assets metadata value structure")]
    AssetMetadataUnexpected,

    #[error("wrong data type")]
    AssetMetadataType,

    #[error("expected a map with a single entry, got multiple entries")]
    AssetMetadataMapSize,

    #[error("asset balance format is unexpected")]
    AssetBalanceFormat,

    #[error("no balance field in an asset record")]
    AssetBalanceNotFound,

    #[error("format of the fetched Base58 prefix {0:?} isn't supported")]
    Base58PrefixFormatNotSupported(String),

    #[error("Base58 prefixes in metadata ({meta:?}) and specs ({specs:?}) do not match.")]
    Base58PrefixMismatch { specs: u16, meta: u16 },

    #[error("unexpected block number format")]
    BlockNumberFormat,

    #[error("unexpected block hash format")]
    BlockHashFormat,

    #[error("unexpected block hash length")]
    BlockHashLength,

    #[error("WS client error is occurred")]
    Client(#[from] ClientError),

    #[error("threading error is occurred")]
    Tokio(#[from] JoinError),

    #[error("format of fetched decimals ({0}) isn't supported")]
    DecimalsFormatNotSupported(String),

    #[error("unexpected genesis hash format")]
    GenesisHashFormat,

    #[error("...")]
    MetadataFormat,

    #[error("...")]
    MetadataNotDecodeable,

    #[error("no Base58 prefix is fetched as system properties or found in metadata")]
    NoBase58Prefix,

    #[error("block number definition isn't found")]
    NoBlockNumberDefinition,

    #[error("no decimals value is fetched")]
    NoDecimals,

    #[error("metadata v15 isn't available through RPC")]
    NoMetadataV15,

    #[error("metadata must start with the `meta` prefix")]
    NoMetaPrefix,

    #[error("pallet isn't found")]
    NoPallet,

    #[error("no pallets with a storage found")]
    NoStorage,

    #[error("\"System\" pallet isn't found")]
    NoSystem,

    #[error("no storage variants in the \"System\" pallet")]
    NoStorageInSystem,

    #[error("no unit value is fetched")]
    NoUnit,

    #[error("...")]
    PropertiesFormat,

    #[error("...")]
    RawMetadataNotDecodeable,

    #[error("format of the fetched unit ({0}) isn't supported")]
    UnitFormatNotSupported(String),

    #[error("unexpected storage value format for the key \"{0:?}\"")]
    StorageValueFormat(Value),

    //#[error("Chain returned zero for block time")]
    //ZeroBlockTime,

    //#[error("Runtime api call response should be String, but received {0:?}")]
    //StateCallResponse(Value),

    //#[error("Could not fetch BABE expected block time")]
    //BabeExpectedBlockTime,

    //#[error("Aura slot duration could not be parsed as u64")]
    //AuraSlotDurationFormat,
    #[error("internal error is occurred")] // TODO this should be replaced by specific errors
    Util(#[from] UtilError),

    #[error("invoice account couldn't be parsed")]
    InvoiceAccount(#[from] CryptoError),

    #[error("chain {0:?} isn't found")]
    InvalidChain(String),

    #[error("currency {0:?} isn't found")]
    InvalidCurrency(String),

    #[error("chain manager dropped a message, probably due to a chain disconnect; maybe it should be sent again")]
    MessageDropped,

    #[error("block subscription is terminated")]
    BlockSubscriptionTerminated,

    #[error("metadata error is occurred")]
    MetaVersionErrorPallets(#[from] MetaVersionErrorPallets),

    #[error("storage registry error is occurred")]
    StorageRegistryError(#[from] StorageRegistryError),

    #[error("balance wasn't found")]
    BalanceNotFound,

    #[error("storage query couldn't be formed")]
    StorageQuery,

    #[error("events couldn't be fetched")]
    EventsMissing,

    #[error("no events in this chain")]
    EventsNonexistant,

    #[error("substrate parser error is occurred")]
    ParserError(#[from] ParserError<()>),

    #[error("storage entry decoding error is occurred")]
    StorageDecodeError(#[from] StorageError<()>),

    #[error("type registry error is occurred")]
    RegistryError(#[from] RegistryError<()>),

    #[error("substrate constructor error is occurred")]
    SubstrateConstructor(#[from] ErrorFixMe<(), RuntimeMetadataV15>),

    #[error("transaction isn't ready to be signed: {0:?}")]
    TransactionNotSignable(String),

    #[error("signing was failed")]
    Signer(#[from] SignerError),

    #[error("transaction couldn't be completed")]
    NothingToSend,

    #[error("storage entry isn't a map")]
    StorageEntryNotMap,

    #[error("storage entry map has more than one record")]
    StorageEntryMapMultiple,

    #[error("storage key {0:?} isn't found")]
    StorageKeyNotFound(String),

    #[error("storage key isn't `u32`")]
    StorageKeyNotU32,

    #[error(
        "RPC runs on an unexpected network: instead of {expected:?}, found {actual:?} at {rpc:?}"
    )]
    WrongNetwork {
        expected: String,
        actual: String,
        rpc: String,
    },
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum DbError {
    #[error("currency key isn't found")]
    CurrencyKeyNotFound,

    #[error("database engine isn't running")]
    DbEngineDown,

    #[error("database internal error is occurred")]
    DbInternalError(#[from] DatabaseError),

    #[error("failed to start the database service")]
    DbStartError(DatabaseError),

    #[error("operating system related I/O error is occurred")]
    IoError(#[from] IoError),

    #[error("database storage decoding error is occurred")]
    CodecError(#[from] ScaleError),

    #[error("order {0:?} isn't found")]
    OrderNotFound(String),

    #[error("order {0:?} was already paid")]
    AlreadyPaid(String),

    #[error("order {0:?} isn't paid yet")]
    NotPaid(String),

    #[error("there was already an attempt to withdraw order {0:?}")]
    WithdrawalWasAttempted(String),
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum OrderError {
    #[error("invoice amount is less than the existential deposit")]
    LessThanExistentialDeposit(f64),

    #[error("unknown currency")]
    UnknownCurrency,

    #[error("order parameter is missing: {0:?}")]
    MissingParameter(String),

    #[error("order parameter invalid: {0:?}")]
    InvalidParameter(String),

    #[error("internal error is occurred")]
    InternalError,
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum ForceWithdrawalError {
    #[error("order parameter is missing: {0:?}")]
    MissingParameter(String),

    #[error("order parameter is invalid: {0:?}")]
    InvalidParameter(String),

    #[error("withdrawal was failed: \"{0:?}\"")]
    WithdrawalError(Box<OrderStatus>),
}

#[derive(Debug, thiserror::Error)]
#[allow(clippy::module_name_repetitions)]
pub enum ServerError {
    #[error("failed to bind the TCP listener to \"{0:?}\"")]
    TcpListenerBind(SocketAddr),

    #[error("internal threading error is occurred")]
    ThreadError,
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum UtilError {
    #[error("...")]
    NotHex(NotHex),
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum SignerError {
    #[error("failed to read {0:?}")]
    Env(String),

    #[error("signer is down")]
    SignerDown,

    #[error("seed phrase is invalid")]
    InvalidSeed(#[from] ErrorWordList),

    #[error("derivation was failed")]
    InvalidDerivation(#[from] CryptoError),
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum NotHex {
    #[error("block hash string isn't a valid hexadecimal")]
    BlockHash,

    #[error("encoded metadata string isn't a valid hexadecimal")]
    Metadata,

    #[error("encoded storage key string isn't a valid hexadecimal")]
    StorageKey,

    #[error("encoded storage value string isn't a valid hexadecimal")]
    StorageValue,
}

pub struct PrettyCauseWrapper<'a, T>(&'a T);

pub trait PrettyCause<T> {
    fn pretty_cause(&self) -> PrettyCauseWrapper<'_, T>;
}

impl<T: StdError> PrettyCause<T> for T {
    fn pretty_cause(&self) -> PrettyCauseWrapper<'_, T> {
        PrettyCauseWrapper(self)
    }
}

impl<T: StdError> Display for PrettyCauseWrapper<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        let Some(cause) = self.0.source() else {
            // If an error has no source, print nothing.
            return Ok(());
        };

        f.write_str("\n\nCaused by:")?;

        let Some(mut another_cause) = cause.source() else {
            // If an error's source error has no source, print a cause in one line.

            f.write_str(" ")?;

            Display::fmt(&cause, f)?;

            return f.write_str(".");
        };
        let mut number = 0u64;

        let mut print_cause = |cause_to_print, number_to_print| {
            f.write_str("\n")?;

            write!(f, "{number_to_print:>5}: ")?;

            Display::fmt(cause_to_print, f)?;

            f.write_str(".")
        };

        // Otherwise, print a numbered list of error sources.

        print_cause(cause, number)?;

        loop {
            number = number.saturating_add(1);

            print_cause(another_cause, number)?;

            if let Some(one_more_cause) = another_cause.source() {
                another_cause = one_more_cause;
            } else {
                break Ok(());
            }
        }
    }
}
