use crate::{arguments::ChainConfig, error::AccountFromStrError};
use arrayvec::ArrayString;
use const_hex::FromHexError;
use jsonrpsee::ws_client::WsClient;
use ruint::aliases::U256;
use serde::{
    de::{Error, Unexpected, Visitor},
    Deserialize, Deserializer,
};
use std::{
    fmt::{Debug, Display, Formatter, Result as FmtResult},
    str::FromStr,
    sync::Arc,
};
use substrate_crypto_light::common::{AccountId32, AsBase58};

#[derive(Debug)]
pub struct ChainConnector(pub Arc<str>);

impl Display for ChainConnector {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.write_str("the \"")?;
        f.write_str(&self.0)?;
        f.write_str("\" chain connector")
    }
}

pub struct ConnectedChain {
    pub config: ChainConfig,
    pub genesis: BlockHash,
    pub client: WsClient,
}

#[derive(Deserialize, Clone, Copy)]
pub struct BlockHash(pub H256);

#[derive(Clone, Copy)]
pub struct H256(pub U256);

impl<'de> Deserialize<'de> for H256 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct H256Visitor;

        impl Visitor<'_> for H256Visitor {
            type Value = H256;

            fn expecting(&self, f: &mut Formatter<'_>) -> FmtResult {
                f.write_str("a hexidecimal 64-character long string prefixed with \"0x\"")
            }

            fn visit_str<E: Error>(self, string: &str) -> Result<Self::Value, E> {
                if let Some(stripped) = string.strip_prefix("0x") {
                    H256::from_hex(stripped).map_err(Error::custom)
                } else {
                    Err(Error::invalid_value(
                        Unexpected::Str(string),
                        &"a string prefixed with \"0x\"",
                    ))
                }
            }
        }

        deserializer.deserialize_str(H256Visitor)
    }
}

// Below are `H256` printing implementations.
// For now, there's 3 option:
// - `{}` prints `0x0000000000000000000000000000000000000000000000000000000000000000`.
// - `{:#}` prints `"0x0000000000000000000000000000000000000000000000000000000000000000"`.
// - `{:?}` prints `H256(0x0000000000000000000000000000000000000000000000000000000000000000)`.

impl Debug for H256 {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_tuple(stringify!(H256))
            .field(&H256Display(self.to_hex()))
            .finish()
    }
}

impl Display for H256 {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let hex = H256Display(self.to_hex());

        if f.alternate() {
            f.write_str("\"")?;
            hex.fmt(f)?;

            f.write_str("\"")
        } else {
            hex.fmt(f)
        }
    }
}

struct H256Display(ArrayString<{ H256::HEX_LENGTH }>);

impl Debug for H256Display {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.write_str("0x")?;

        f.write_str(&self.0)
    }
}

impl H256 {
    pub const HEX_LENGTH: usize = U256::BYTES * 2;

    pub fn from_hex(string: impl AsRef<[u8]>) -> Result<Self, FromHexError> {
        const_hex::decode_to_array(string.as_ref())
            .map(|array| Self(U256::from_be_bytes::<{ U256::BYTES }>(array)))
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn to_hex(&self) -> ArrayString<{ Self::HEX_LENGTH }> {
        let mut array = [0; Self::HEX_LENGTH];

        const_hex::encode_to_slice(self.to_be_bytes(), &mut array).unwrap();
        ArrayString::from_byte_string(&array).unwrap()
    }

    pub fn from_be_bytes(bytes: impl Into<[u8; U256::BYTES]>) -> Self {
        Self(U256::from_be_bytes(bytes.into()))
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn to_be_bytes(&self) -> [u8; U256::BYTES] {
        self.0.to_le_bytes()
    }
}

pub struct Account(u16, [u8; 32]);

impl FromStr for Account {
    type Err = AccountFromStrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        const SUBSTRATE_PREFIX: u16 = 42;

        let (account, prefix) = if let Some(stripped) = s.strip_prefix("0x") {
            H256::from_hex(stripped).map(|hash| (hash.to_be_bytes(), SUBSTRATE_PREFIX))?
        } else {
            AccountId32::from_base58_string(s).map(|(account, p)| (account.0, p))?
        };

        Ok(Self(prefix, account))
    }
}

// #[derive(Clone, Copy)]
// pub struct RootChainIntervals {
//     restart_gap: Option<ChainInterval>,
//     account_lifetime: Option<ChainInterval>,
// }

// impl RootChainIntervals {
//     pub fn new(config: &ArgChainIntervals) -> Result<Self, Error> {
//         Ok(Self {
//             restart_gap: ChainInterval::root(config.restart_gap, config.restart_gap_in_blocks)
//                 .map_err(|e| Error::ConfigRootIntervals(ChainIntervalField::RestartGap, e))?,
//             account_lifetime: ChainInterval::root(
//                 config.account_lifetime,
//                 config.account_lifetime_in_blocks,
//             )
//             .map_err(|e| Error::ConfigRootIntervals(ChainIntervalField::AccountLifetime, e))?,
//         })
//     }
// }

// pub struct ChainIntervals {
//     pub restart_gap: ChainInterval,
//     pub account_lifetime: ChainInterval,
// }

// impl ChainIntervals {
//     pub fn new(root: RootChainIntervals, chain: &ArgChainIntervals) -> Result<Self, TaskError> {
//         Ok(Self {
//             restart_gap: ChainInterval::chain(root, chain.restart_gap, chain.restart_gap_in_blocks)
//                 .map_err(|e| TaskError::ChainInterval(ChainIntervalField::RestartGap, e))?,
//             account_lifetime: ChainInterval::chain(
//                 root,
//                 chain.account_lifetime,
//                 chain.account_lifetime_in_blocks,
//             )
//             .map_err(|e| TaskError::ChainInterval(ChainIntervalField::AccountLifetime, e))?,
//         })
//     }
// }

// #[derive(Clone, Copy)]
// pub enum ChainInterval {
//     Time(Timestamp),
//     Blocks(BlockNumber),
// }

// impl ChainInterval {
//     fn new(
//         time: Option<Timestamp>,
//         blocks: Option<BlockNumber>,
//     ) -> Result<Self, ChainIntervalError> {
//         match (time, blocks) {
//             (None, None) => Err(ChainIntervalError::NotSet),
//             (Some(_), Some(_)) => Err(ChainIntervalError::DoubleSet),
//             (None, Some(b)) => Ok(Self::Blocks(b)),
//             (Some(t), None) => Ok(Self::Time(t)),
//         }
//     }

//     fn chain(
//         root: RootChainIntervals,
//         time: Option<Timestamp>,
//         blocks: Option<BlockNumber>,
//     ) -> Result<Self, ChainIntervalError> {
//         match ChainInterval::new(time, blocks) {
//             Ok(ci) => Ok(ci),
//             Err(error @ ChainIntervalError::NotSet) => root.restart_gap.ok_or(error),
//             Err(error @ ChainIntervalError::DoubleSet) => Err(error),
//         }
//     }

//     fn root(
//         time: Option<Timestamp>,
//         blocks: Option<BlockNumber>,
//     ) -> Result<Option<Self>, ChainIntervalError> {
//         match Self::new(time, blocks) {
//             Ok(ci) => Ok(Some(ci)),
//             Err(ChainIntervalError::NotSet) => Ok(None),
//             Err(error @ ChainIntervalError::DoubleSet) => Err(error),
//         }
//     }
// }

// #[derive(Debug)]
// pub enum ChainIntervalField {
//     RestartGap,
//     AccountLifetime,
// }

// impl Display for ChainIntervalField {
//     fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
//         f.write_str(match self {
//             ChainIntervalField::RestartGap => "\"restart-gap\"",
//             ChainIntervalField::AccountLifetime => "\"account-lifetime\"",
//         })
//     }
// }
