use crate::{definitions::api_v2::OrderStatus, SocketAddr};
use frame_metadata::v15::RuntimeMetadataV15;
use jsonrpsee::core::client::error::Error as ClientError;
use mnemonic_external::error::ErrorWordList;
use primitive_types::H256;
use serde_json::Value;
use sled::Error as DatabaseError;
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

    #[error("Failed to listen to shutdown signal")]
    ShutdownSignal,

    #[error("Receiver account could not be parsed: {0:?}")]
    RecipientAccount(substrate_crypto_light::error::Error),

    #[error("Seed phrase invalid: {0:?}")]
    InvalidSeed(ErrorWordList),

    #[error("Derivation failed: {0:?}")]
    InvalidDerivation(CryptoError),

    #[error("Fatal error. System is shutting down.")]
    Fatal,

    #[error("Operating system related I/O error {0:?}")]
    IoError(std::io::Error),
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

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::IoError(e)
    }
}

impl From<ErrorWordList> for Error {
    fn from(e: ErrorWordList) -> Self {
        Error::InvalidSeed(e)
    }
}

impl From<CryptoError> for Error {
    fn from(e: CryptoError) -> Self {
        Error::InvalidDerivation(e)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ErrorChain {
    #[error("Format of fetched base58 prefix {value} is not supported.")]
    Base58PrefixFormatNotSupported { value: String },

    #[error("Base58 prefixes in metadata {meta} and specs {specs} do not match.")]
    Base58PrefixMismatch { specs: u16, meta: u16 },

    #[error("Unexpected block hash format.")]
    BlockHashFormat,

    #[error("Unexpected block hash length.")]
    BlockHashLength,

    #[error("Ws client error. {0}")]
    Client(ClientError),

    #[error("Threading error. {0}")]
    Tokio(JoinError),

    //    #[error("Internal database error. {0}")]
    //    DbInternal(sled::Error),

    //    #[error("Database error recording transaction. {0}")]
    //    DbTransaction(sled::transaction::TransactionError),
    #[error("Format of fetched decimals {value} is not supported.")]
    DecimalsFormatNotSupported { value: String },

    #[error("Fetch address in the database for genesis hash {} got damaged, and could not be decoded.", hex::encode(.0))]
    DecodeDbAddress(H256),

    //#[error("Key in the database {} is damaged, and could not be decoded.", hex::encode(.0))]
    //DecodeDbKey(IVec),
    #[error("MetadataSpecs in the database for genesis hash {} got damaged, and could not be decoded.", hex::encode(.0))]
    DecodeDbMetadataSpecs(H256),

    #[error("Unexpected genesis hash format.")]
    GenesisHashFormat,

    #[error("Unexpected genesis hash length.")]
    GenesisHashLength,

    #[error("Key {0} got damaged on the interface.")]
    InterfaceKey(String),

    #[error(
        "No metadata and specs for chain with genesis hash {} in the database.",
        hex::encode(genesis_hash)
    )]
    LoadSpecsMetadata { genesis_hash: H256 },

    #[error("Address for chain with genesis hash {} is not in the database.", hex::encode(.0))]
    LostAddress(H256),

    #[error("Metadata fetch was somehow done with no pre-existing entry for genesis hash {}. This is a bug, please report it.", hex::encode(.0))]
    MetadataFetchWithoutExistingEntry(H256),

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
