use crate::{
    arguments::{OLD_SEED, SEED},
    chain::definitions::H256,
    database::{
        definitions::{BlockHash, Public, Timestamp, Version},
        DB_VERSION,
    },
    definitions::api_v2::OrderStatus,
    utils::task_tracker::TaskName,
};
use codec::Error as CodecError;
use const_hex::FromHexError;
use frame_metadata::v15::RuntimeMetadataV15;
use jsonrpsee::core::ClientError;
use mnemonic_external::error::ErrorWordList;
use redb::{
    CommitError, CompactionError, DatabaseError, StorageError as DbStorageError, TableError,
    TransactionError,
};
use serde_json::Error as JsonError;
use serde_json::Value;
use sled::Error as DatabaseErrorr;
use std::{io::Error as IoError, net::SocketAddr, str::Utf8Error};
use substrate_constructor::error::{ErrorFixMe, StorageRegistryError};
use substrate_crypto_light::error::Error as CryptoError;
use substrate_parser::error::{MetaVersionErrorPallets, ParserError, RegistryError, StorageError};
use thiserror::Error;
use tokio::task::JoinError;
use toml_edit::de::Error as TomlError;
use tracing_subscriber::filter::ParseError;

pub use pretty_cause::PrettyCause;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to read a seed environment variable")]
    SeedEnv(#[from] SeedEnvError),

    #[error("failed to read the config file at {0:?}")]
    ConfigFileRead(String, #[source] IoError),

    #[error("failed to parse the config")]
    ConfigFileParse(#[from] TomlError),

    #[error("failed to parse given filter directives for the logger ({0:?})")]
    LoggerDirectives(String, #[source] ParseError),

    #[error("failed to initialize the asynchronous runtime")]
    Runtime(#[source] IoError),

    #[error("failed to listen for the shutdown signal")]
    ShutdownSignal(#[source] IoError),

    #[error("failed to complete {0}")]
    Task(TaskName, #[source] TaskError),

    #[error("got a database initialization error")]
    Database(#[from] DbError),

    #[error("failed to parse the recipient parameter")]
    RecipientParse(#[from] AccountFromStrError),

    // #[error("failed to parse the chain interval {0} in the config root")]
    // ConfigRootIntervals(ChainIntervalField, #[source] ChainIntervalError),
    #[error("failed to parse the config parameter `{0}`")]
    ConfigParse(&'static str),

    #[error("chain {0:?} doesn't have any `endpoints` in the config")]
    EmptyEndpoints(String),

    #[error("RPC server error is occurred")]
    Chain(#[from] ChainError),

    #[error("database error is occurred")]
    Db(#[from] DbErrorr),

    #[error("order error is occurred")]
    Order(#[from] OrderError),

    #[error("daemon server error is occurred")]
    Server(#[from] ServerError),

    #[error("signer error is occurred")]
    Signer(#[from] SignerError),

    #[error("receiver account couldn't be parsed")]
    RecipientAccount(#[from] CryptoError),

    #[error("fatal error is occurred")]
    Fatal,

    #[error("found duplicate config record for the token {0:?}")]
    DuplicateCurrency(String),
}

const SEED_ENV_INVALID_UNICODE: &str = "` variable contains an invalid Unicode text";

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum SeedEnvError {
    #[error("one of the `{OLD_SEED}*` variables has an invalid Unicode text in the name")]
    InvalidUnicodeOldKey(#[from] Utf8Error),
    #[error("`{SEED}{SEED_ENV_INVALID_UNICODE}")]
    InvalidUnicodeValue,
    #[error("`{OLD_SEED}{0}{SEED_ENV_INVALID_UNICODE}")]
    InvalidUnicodeOldValue(String),
    #[error("`{SEED}` is required & not set")]
    NotSet,
    #[error("failed to parse a mnemonic phrase")]
    Mnemonic(#[from] ErrorWordList),
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum TaskError {
    #[error("chain has no endpoint in the config")]
    NoChainEndpoints,
    #[error("got an RPC error")]
    Rpc(#[from] RpcError),
    #[error("found 2 chains with the same name in the config (see the connector name above)")]
    ChainDuplicate,
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum ChainIntervalError {
    #[error("chain interval isn't set")]
    NotSet,
    #[error("received 2 values for an interval parameter that expects only 1, choose between blocks (`*_in_blocks`) & time")]
    DoubleSet,
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum RpcError {
    #[error("failed to construct a connection to an RPC endpoint")]
    Connection(#[source] ClientError),
    #[error("failed to fetch the genesis hash")]
    GenesisHash(#[source] ClientError),
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

    #[error("failed to parse JSON data from a block stream")]
    Serde(#[from] JsonError),
}

const ACCOUNT_FROM_STR_FAILED: &str = "failed to parse an address string in the ";

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum AccountFromStrError {
    #[error("{ACCOUNT_FROM_STR_FAILED}hexadecimal format")]
    Hex(#[from] FromHexError),

    #[error("{ACCOUNT_FROM_STR_FAILED}SS58 format")]
    Ss58(#[from] CryptoError),
}

const DB_ERROR_NO_VALUE: &str = "existing database doesn't contain ";

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum DbError {
    #[error("failed to create/open the database")]
    Initialization(#[from] DatabaseError),

    #[error("failed to create a write transaction")]
    WriteTx(#[from] TransactionError),

    #[error("failed to commit a transaction")]
    Commit(#[from] CommitError),

    #[error("failed to compact the database")]
    Compact(#[from] CompactionError),

    #[error("failed to open a table")]
    OpenTable(#[source] TableError),

    #[error("failed to delete a table")]
    DeleteTable(#[source] TableError),

    #[error("failed to insert a value into the database")]
    Insert(#[source] DbStorageError),

    #[error("failed to get a value from the database")]
    Get(#[source] DbStorageError),

    #[error("{DB_ERROR_NO_VALUE}its format version")]
    NoVersion,

    #[error("{DB_ERROR_NO_VALUE}the daemon info")]
    NoDaemonInfo,

    #[error("database contains an invalid format version ({}), expected {}", .0 .0, DB_VERSION.0)]
    UnexpectedVersion(Version),

    #[error("failed to decode a SCALE-encoded value")]
    Codec(#[from] CodecError),

    #[error("failed to get list of tables")]
    TableList(#[source] DbStorageError),

    #[error(
        "chain {chain:?} has different genesis hashes in the database ({:#}) & from an RPC \
        server ({:#}), try to check RPC server URLs in the config for correctness",
        H256::from(*expected),
        H256::from(*given)
    )]
    GenesisMismatch {
        chain: String,
        expected: BlockHash,
        given: BlockHash,
    },

    #[error(
        "public key {:#} that was removed on {removed} has no matching seed among `{OLD_SEED}*`",
        H256::from(*key)
    )]
    OldKeyNotFound { key: Public, removed: Timestamp },

    #[error("current key {:#} has no matching seed among `{OLD_SEED}*`", H256::from(*.0))]
    CurrentKeyNotFound(Public),

    #[error(
        "current system time is too far in the past or the future, \
        check the system time for correctness"
    )]
    AbnormalSystemTime,
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum DbErrorr {
    #[error("currency key isn't found")]
    CurrencyKeyNotFound,

    #[error("database engine isn't running")]
    DbEngineDown,

    #[error("database internal error is occurred")]
    DbInternalError(#[from] DatabaseErrorr),

    #[error("failed to start the database service")]
    DbStartError(DatabaseErrorr),

    #[error("operating system related I/O error is occurred")]
    IoError(#[from] IoError),

    #[error("database storage decoding error is occurred")]
    CodecError(#[from] CodecError),

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

mod pretty_cause {
    use std::{
        error::Error,
        fmt::{Display, Formatter, Result},
    };

    const OVERLOAD: u16 = 9999;

    pub struct Wrapper<'a, T>(&'a T);

    pub trait PrettyCause<T> {
        fn pretty_cause(&self) -> Wrapper<'_, T>;
    }

    impl<T: Error> PrettyCause<T> for T {
        fn pretty_cause(&self) -> Wrapper<'_, T> {
            Wrapper(self)
        }
    }

    impl<T: Error> Display for Wrapper<'_, T> {
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

            // Otherwise, print a numbered list of error sources.

            let mut number = 0u16;

            print_cause(f, cause, number)?;

            loop {
                if number == OVERLOAD {
                    break;
                }

                number = number.saturating_add(1);

                print_cause(f, another_cause, number)?;

                if let Some(one_more_cause) = another_cause.source() {
                    another_cause = one_more_cause;
                } else {
                    return Ok(());
                }
            }

            loop {
                print_cause(f, another_cause, shadow_rs::formatcp!(">{OVERLOAD}"))?;

                if let Some(one_more_cause) = another_cause.source() {
                    another_cause = one_more_cause;
                } else {
                    break Ok(());
                }
            }
        }
    }

    fn print_cause(
        f: &mut Formatter<'_>,
        cause: &(impl Error + ?Sized),
        number: impl Display,
    ) -> Result {
        f.write_str("\n")?;

        write!(f, "{number:>5}")?;

        f.write_str(": ")?;

        Display::fmt(cause, f)?;

        f.write_str(".")
    }

    #[cfg(test)]
    mod tests {
        use super::{PrettyCause, OVERLOAD};
        use std::{
            error::Error,
            fmt::{Debug, Display, Formatter, Result, Write},
        };

        #[test]
        fn empty() {
            assert!(TestError::empty().pretty_cause().to_string().is_empty());
        }

        #[test]
        fn single() {
            const MESSAGE: &str = "\n\nCaused by: TestError(0).";

            assert_eq!(TestError::nested(1).pretty_cause().to_string(), MESSAGE);
        }

        #[test]
        fn multiple() {
            const MESSAGE: &str = indoc::indoc! {"
                \n\nCaused by:
                    0: TestError(2).
                    1: TestError(1).
                    2: TestError(0)."
            };

            assert_eq!(TestError::nested(3).pretty_cause().to_string(), MESSAGE);
        }

        #[test]
        fn overload() {
            let message = TestError::nested(OVERLOAD.saturating_add(5))
                .pretty_cause()
                .to_string();
            let mut expected_message = String::with_capacity(message.len());

            expected_message.push_str("\n\nCaused by:");

            for number in 0..=OVERLOAD {
                write!(
                    expected_message,
                    "\n{number:>5}: {}.",
                    TestError {
                        source: None,
                        number: OVERLOAD + 4 - number
                    }
                )
                .unwrap();
            }

            expected_message.push_str(indoc::indoc! {"
                \n>9999: TestError(3).
                >9999: TestError(2).
                >9999: TestError(1).
                >9999: TestError(0)."
            });

            assert_eq!(message, expected_message);
        }

        #[derive(Debug)]
        struct TestError {
            source: Option<Box<TestError>>,
            number: u16,
        }

        impl TestError {
            fn empty() -> Self {
                Self {
                    source: None,
                    number: 0,
                }
            }

            fn nested(nest: u16) -> Self {
                let mut e = Self::empty();

                for _ in 0..nest {
                    e = Self {
                        number: e.number.saturating_add(1),
                        source: Some(e.into()),
                    };
                }

                e
            }
        }

        impl Display for TestError {
            fn fmt(&self, f: &mut Formatter<'_>) -> Result {
                f.debug_tuple(stringify!(TestError))
                    .field(&self.number)
                    .finish()
            }
        }

        impl Error for TestError {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                self.source.as_ref().map(|e| e as _)
            }
        }
    }
}
