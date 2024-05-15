use crate::{definitions::api_v2::OrderStatus, SocketAddr};
use frame_metadata::v15::RuntimeMetadataV15;
use jsonrpsee::core::client::error::Error as ClientError;
use mnemonic_external::error::ErrorWordList;
use primitive_types::H256;
use serde_json::Value;
use sled::Error as DatabaseError;
use substrate_constructor::error::*;
use substrate_crypto_light::error::Error as CryptoError;
use substrate_parser::error::*;
use tokio::task::JoinError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read `{0}`")]
    Env(String),

    #[error("failed to read a config file at {0:?}")]
    ConfigFileRead(String),

    #[error("failed to parse the config at {0:?}")]
    ConfigFileParse(String),

    #[error("failed to parse the config parameter {0}")]
    ConfigParse(String),

    #[error("RPC server error: {0:?}")]
    ErrorChain(ErrorChain),

    #[error("Database error: {0:?}")]
    ErrorDb(ErrorDb),

    #[error("Order error: {0:?}")]
    ErrorOrder(ErrorOrder),

    #[error("Daemon server error: {0:?}")]
    ErrorServer(ErrorServer),

    #[error("Signer error: {0:?}")]
    ErrorSigner(ErrorSigner),

    #[error("Failed to listen to shutdown signal")]
    ShutdownSignal,

    #[error("Receiver account could not be parsed: {0:?}")]
    RecipientAccount(substrate_crypto_light::error::Error),

    #[error("Fatal error. System is shutting down.")]
    Fatal,

    #[error("Operating system related I/O error {0:?}")]
    IoError(std::io::Error),

    #[error("Duplicate config record for token {0}")]
    DuplicateCurrency(String),
}

impl From<ErrorDb> for Error {
    fn from(e: ErrorDb) -> Error {
        Error::ErrorDb(e)
    }
}

impl From<ErrorChain> for Error {
    fn from(e: ErrorChain) -> Error {
        Error::ErrorChain(e)
    }
}

impl From<ErrorOrder> for Error {
    fn from(e: ErrorOrder) -> Error {
        Error::ErrorOrder(e)
    }
}

impl From<ErrorServer> for Error {
    fn from(e: ErrorServer) -> Error {
        Error::ErrorServer(e)
    }
}

impl From<ErrorSigner> for Error {
    fn from(e: ErrorSigner) -> Error {
        Error::ErrorSigner(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::IoError(e)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ErrorChain {
    #[error("Asset id is not u32")]
    AssetIdFormat,

    #[error("Asset key has no parceable part")]
    AssetKeyEmpty,

    #[error("Asset key is not single hash")]
    AssetKeyNotSingleHash,

    #[error("Asset metadata is not a map")]
    AssetMetadataPlain,

    #[error("unexpected assets metadata value structure")]
    AssetMetadataUnexpected,

    #[error("wrong data type")]
    AssetMetadataType,

    #[error("expected map with single entry, got multiple entries")]
    AssetMetadataMapSize,

    #[error("Format of fetched base58 prefix {value} is not supported.")]
    Base58PrefixFormatNotSupported { value: String },

    #[error("Base58 prefixes in metadata {meta} and specs {specs} do not match.")]
    Base58PrefixMismatch { specs: u16, meta: u16 },

    #[error("Unexpected block number format.")]
    BlockNumberFormat,

    #[error("Unexpected block hash format.")]
    BlockHashFormat,

    #[error("Unexpected block hash length.")]
    BlockHashLength,

    #[error("Ws client error. {0}")]
    Client(ClientError),

    #[error("Threading error. {0}")]
    Tokio(JoinError),

    #[error("Format of fetched decimals {value} is not supported.")]
    DecimalsFormatNotSupported { value: String },

    #[error("Unexpected genesis hash format.")]
    GenesisHashFormat,

    #[error("Unexpected genesis hash length.")]
    GenesisHashLength,

    #[error("...")]
    MetadataFormat,

    #[error("...")]
    MetadataNotDecodeable,

    #[error("{0}")]
    MetadataVersion(MetaVersionErrorPallets),

    #[error("No base58 prefix is fetched as system properties or found in the metadata.")]
    NoBase58Prefix,

    #[error("No decimals value is fetched.")]
    NoDecimals,

    #[error("No existing metadata and specs entry before metadata update for hash {}. Remove entry and start over.", hex::encode(.0))]
    NoExistingEntryMetadataUpdate(H256),

    #[error("Metadata v15 not available through rpc.")]
    NoMetadataV15,

    #[error("Metadata must start with `meta` prefix.")]
    NoMetaPrefix,

    #[error("{0}")]
    NotHex(NotHex),

    #[error("Fetched values were not sent through successfully.")]
    NotSent,

    #[error("No unit value is fetched.")]
    NoUnit,

    #[error("Only Sr25519 encryption, 0x01, is supported. Received transaction has encoded encryption 0x{}", hex::encode([*.0]))]
    OnlySr25519(u8),

    #[error("...")]
    PoisonedLockSelector,

    #[error("...")]
    PropertiesFormat,

    #[error("...")]
    RawMetadataNotDecodeable,

    #[error("Can't read data through the interface. Receiver closed.")]
    ReceiverClosed,

    #[error("Can't read data through the interface. Receiver guard is poisoned.")]
    ReceiverGuardPoisoned,

    #[error("Received QR payload is too short.")]
    TooShort,

    #[error("Received transaction could not be parsed. {0}.")]
    TransactionNotParsable(SignableError<(), RuntimeMetadataV15>),

    #[error("Unexpected payload type, 0x{}", hex::encode([*.0]))]
    UnknownPayloadType(u8),

    #[error("Format of fetched unit {value} is not supported.")]
    UnitFormatNotSupported { value: String },

    #[error("Try updating metadata. Metadata version in transaction {as_decoded} does not match the version of the available metadata entry {in_metadata}.")]
    UpdateMetadata {
        as_decoded: String,
        in_metadata: String,
    },

    #[error("Unexpected storage value format for key {0}")]
    StorageValueFormat(String),

    #[error("Chain returned zero for block time")]
    ZeroBlockTime,

    #[error("chain doesn't have any `endpoints` in the config")]
    EmptyEndpoints,

    #[error("Runtime api call response should be String, but received {0:?}")]
    StateCallResponse(Value),

    #[error("Could not fetch BABE expected block time")]
    BabeExpectedBlockTime,

    #[error("Aura slot duration could not be parsed as u64")]
    AuraSlotDurationFormat,

    #[error("Internal error: {0:?}")] // TODO this should be replaced by specific errors
    ErrorUtil(ErrorUtil),

    #[error("Invoice account could not be parsed: {0:?}")]
    InvoiceAccount(substrate_crypto_light::error::Error),

    #[error("Chain {0} not found")]
    InvalidChain(String),

    #[error("Currency {0} not found")]
    InvalidCurrency(String),

    #[error("Chain manager dropped a message, probably due to chain disconnect; maybe it should be sent again")]
    MessageDropped,

    #[error("Block subscription terminated")]
    BlockSubscriptionTerminated,

    #[error("Metadata error: {0:?}")]
    MetaVersionErrorPallets(MetaVersionErrorPallets),

    #[error("Storage registry error: {0:?}")]
    StorageRegistryError(StorageRegistryError),

    #[error("Balance was not found")]
    BalanceNotFound,

    #[error("Storage query could not be formed")]
    StorageQuery,

    #[error("Storage format error")]
    StorageFormatError,

    #[error("Events could not be fetched")]
    EventsMissing,

    #[error("Events do not exist in this chain")]
    EventsNonexistant,

    #[error("Substrate parser error: {0}")]
    ParserError(ParserError<()>),

    #[error("Storage entry decoding error: {0}")]
    StorageDecodeError(StorageError<()>),
}

impl From<ClientError> for ErrorChain {
    fn from(e: ClientError) -> Self {
        ErrorChain::Client(e)
    }
}

impl From<JoinError> for ErrorChain {
    fn from(e: JoinError) -> Self {
        ErrorChain::Tokio(e)
    }
}

impl From<ErrorUtil> for ErrorChain {
    fn from(e: ErrorUtil) -> Self {
        ErrorChain::ErrorUtil(e)
    }
}

impl From<MetaVersionErrorPallets> for ErrorChain {
    fn from(e: MetaVersionErrorPallets) -> Self {
        ErrorChain::MetaVersionErrorPallets(e)
    }
}

impl From<StorageRegistryError> for ErrorChain {
    fn from(e: StorageRegistryError) -> Self {
        ErrorChain::StorageRegistryError(e)
    }
}

impl From<ParserError<()>> for ErrorChain {
    fn from(e: ParserError<()>) -> Self {
        ErrorChain::ParserError(e)
    }
}

impl From<StorageError<()>> for ErrorChain {
    fn from(e: StorageError<()>) -> Self {
        ErrorChain::StorageDecodeError(e)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ErrorDb {
    #[error("Currency key is not found")]
    CurrencyKeyNotFound,

    #[error("Database engine is not running")]
    DbEngineDown,

    #[error("Database internal error: {0:?}")]
    DbInternalError(DatabaseError),

    #[error("Could not start database service: {0:?}")]
    DbStartError(DatabaseError),

    #[error("Operating system related I/O error {0:?}")]
    IoError(std::io::Error),

    #[error("Database storage decoding error: {0:?}")]
    CodecError(parity_scale_codec::Error),

    #[error("Order {0} not found")]
    OrderNotFound(String),

    #[error("Order {0} was already paid")]
    AlreadyPaid(String),

    #[error("Order {0} is not paid yet")]
    NotPaid(String),

    #[error("There was already an attempt to withdraw order {0}")]
    WithdrawalWasAttempted(String),
}

impl From<sled::Error> for ErrorDb {
    fn from(e: sled::Error) -> Self {
        ErrorDb::DbInternalError(e)
    }
}

impl From<parity_scale_codec::Error> for ErrorDb {
    fn from(e: parity_scale_codec::Error) -> Self {
        ErrorDb::CodecError(e)
    }
}

impl From<std::io::Error> for ErrorDb {
    fn from(e: std::io::Error) -> Self {
        ErrorDb::IoError(e)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ErrorOrder {
    #[error("Invoice amount is less than existential deposit")]
    LessThanExistentialDeposit(f64),

    #[error("Unknown currency")]
    UnknownCurrency,

    #[error("Order parameter missing: {0}")]
    MissingParameter(String),

    #[error("Order parameter invalid: {0}")]
    InvalidParameter(String),

    #[error("Order already processed: {0:?}")]
    AlreadyProcessed(Box<OrderStatus>),

    #[error("Internal error")]
    InternalError,
}

#[derive(Debug, thiserror::Error)]
pub enum ErrorForceWithdrawal {
    #[error("Order parameter missing: {0}")]
    MissingParameter(String),

    #[error("Order parameter invalid: {0}")]
    InvalidParameter(String),

    #[error("Withdrawal failed: {0:?}")]
    WithdrawalError(OrderStatus),
}

#[derive(Debug, thiserror::Error)]
pub enum ErrorServer {
    #[error("failed to bind the TCP listener to {0:?}")]
    TcpListenerBind(SocketAddr),

    #[error("Internal threading error")]
    ThreadError,
}

#[derive(Debug, thiserror::Error)]
pub enum ErrorUtil {
    #[error("{0}")]
    NotHex(NotHex),
}

#[derive(Debug, thiserror::Error)]
pub enum ErrorSigner {
    #[error("failed to read `{0}`")]
    Env(String),

    #[error("Signer is down")]
    SignerDown,

    #[error("Seed phrase invalid: {0:?}")]
    InvalidSeed(ErrorWordList),

    #[error("Derivation failed: {0:?}")]
    InvalidDerivation(CryptoError),
}

impl From<ErrorWordList> for ErrorSigner {
    fn from(e: ErrorWordList) -> Self {
        ErrorSigner::InvalidSeed(e)
    }
}

impl From<CryptoError> for ErrorSigner {
    fn from(e: CryptoError) -> Self {
        ErrorSigner::InvalidDerivation(e)
    }
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum NotHex {
    #[error("Block hash string is not a valid hexadecimal.")]
    BlockHash,

    #[error("Genesis hash string is not a valid hexadecimal.")]
    GenesisHash,

    #[error("Encoded metadata string is not a valid hexadecimal.")]
    Metadata,

    #[error("Encoded storage value string is not a valid hexadecimal.")]
    StorageValue,
}
