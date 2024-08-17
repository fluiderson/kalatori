//! Common objects for the chain interaction system.

use crate::{
    arguments::{ChainConfigInner, ChainIntervals as ArgChainIntervals},
    database::definitions::{BlockNumber, Timestamp},
    error::{AccountFromStrError, ChainIntervalError, Error, TaskError},
};
use ahash::RandomState;
use arrayvec::ArrayString;
use const_hex::FromHexError;
use indexmap::IndexSet;
use jsonrpsee::ws_client::{WsClient, WsClientBuilder};
use ruint::aliases::U256;
use serde::{
    de::{Error as DeError, Unexpected, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::{
    fmt::{Debug, Display, Formatter, Result as FmtResult},
    hash::Hash,
    str::FromStr,
    sync::Arc,
};
use substrate_crypto_light::common::{AccountId32, AsBase58};

const HEX_PREFIX: &str = "0x";

#[derive(Debug)]
pub struct ChainPreparator(pub Arc<str>);

impl Display for ChainPreparator {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.write_str("the ")?;

        f.double(|formatter| formatter.write_str(&self.0))?;

        f.write_str(" chain preparator")
    }
}

pub struct ConnectedChain {
    pub endpoints: (String, IndexSet<String, RandomState>),
    pub config: ChainConfigInner,
    pub genesis: BlockHash,
    pub rpc_client: WsClient,
    pub rpc_config: WsClientBuilder,
}

#[derive(Deserialize, Clone, Copy, Serialize)]
pub struct BlockHash(pub H256);

macro_rules! hash_impl {
    ($name:ident, $inner:ident, $bytes:expr) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub $inner);

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct HashVisitor;

                impl Visitor<'_> for HashVisitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut Formatter<'_>) -> FmtResult {
                        write!(
                            f,
                            "a hexidecimal {}-character long string prefixed with {HEX_PREFIX:?}",
                            $name::HEX_LENGTH
                        )
                    }

                    fn visit_str<E: DeError>(self, string: &str) -> Result<Self::Value, E> {
                        decode_hex_for_visitor(string, |stripped| $name::from_hex(stripped))
                    }
                }

                deserializer.deserialize_str(HashVisitor)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_prefixed_hex())
            }
        }

        // `$name` printing implementations:
        // - `{}` prints `0x00000000...`.
        // - `{:#}` prints `"0x00000000..."`.
        // - `{:?}` prints `$name(0x00000000...)`.
        paste::paste! {
            impl Debug for $name {
                fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
                    f.debug_tuple(stringify!($name)).field(&[<$name Display>](self)).finish()
                }
            }

            impl Display for $name {
                fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
                    let hex = [<$name Display>](self);

                    if f.alternate() {
                        f.double(|formatter| hex.fmt(formatter))
                    } else {
                        hex.fmt(f)
                    }
                }
            }

            struct [<$name Display>]<'a>(&'a $name);

            impl Debug for [<$name Display>]<'_> {
                fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
                    f.write_str(&self.0.to_prefixed_hex())
                }
            }
        }

        impl $name {
            pub const HEX_LENGTH: usize = $bytes * 2;

            pub fn from_hex(string: impl AsRef<[u8]>) -> Result<Self, FromHexError> {
                const_hex::decode_to_array(string.as_ref()).map(|array| Self::from_be_bytes(array))
            }

            #[allow(clippy::wrong_self_convention)]
            pub fn to_hex(&self) -> ArrayString<{ Self::HEX_LENGTH }> {
                let mut array = [0; Self::HEX_LENGTH];

                const_hex::encode_to_slice(self.to_be_bytes(), &mut array).unwrap();
                ArrayString::from_byte_string(&array).unwrap()
            }

            #[allow(clippy::wrong_self_convention)]
            pub fn to_prefixed_hex(&self) -> ArrayString<{ Self::HEX_LENGTH + HEX_PREFIX.len() }> {
                let mut array = [0; Self::HEX_LENGTH + 2];
                let (prefix, hex) = array.split_at_mut(HEX_PREFIX.len());

                prefix.copy_from_slice(HEX_PREFIX.as_bytes());

                const_hex::encode_to_slice(self.to_be_bytes(), hex).unwrap();
                ArrayString::from_byte_string(&array).unwrap()
            }

            pub fn from_be_bytes(bytes: impl Into<[u8; $bytes]>) -> Self {
                Self($inner::from_be_bytes(bytes.into()))
            }

            #[allow(clippy::wrong_self_convention)]
            pub fn to_be_bytes(&self) -> [u8; $bytes] {
                self.0.to_be_bytes()
            }
        }
    };
}

hash_impl!(H256, U256, U256::BYTES);
hash_impl!(H64, u64, (u64::BITS / 8) as usize);

#[derive(Clone, Copy)]
pub enum Account {
    Hex(AccountId32),
    Substrate(SS58Prefix, AccountId32),
}

impl FromStr for Account {
    type Err = AccountFromStrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if let Some(stripped) = s.strip_prefix(HEX_PREFIX) {
            H256::from_hex(stripped).map(|hash| Self::Hex(AccountId32(hash.to_be_bytes())))?
        } else {
            AccountId32::from_base58_string(s)
                .map(|(account, p)| Self::Substrate(SS58Prefix(p), account))?
        })
    }
}

impl Display for Account {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Hex(a) => Display::fmt(&H256::from_be_bytes(a.0), f),
            Self::Substrate(p, a) => {
                let s = &a.to_base58_string(p.0);

                if f.alternate() {
                    Debug::fmt(s, f)
                } else {
                    Display::fmt(s, f)
                }
            }
        }
    }
}

impl From<Account> for AccountId32 {
    fn from(value: Account) -> Self {
        match value {
            Account::Hex(a) | Account::Substrate(_, a) => a,
        }
    }
}

#[derive(Clone, Copy)]
pub struct SS58Prefix(pub u16);

impl From<u16> for SS58Prefix {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy)]
pub struct RootChainIntervals {
    restart_gap: Option<ChainInterval>,
    account_lifetime: Option<ChainInterval>,
}

impl RootChainIntervals {
    pub fn new(intervals: &ArgChainIntervals) -> Result<Self, Error> {
        Ok(Self {
            restart_gap: ChainInterval::root(
                intervals.restart_gap,
                intervals.restart_gap_in_blocks,
            )
            .map_err(|e| Error::ConfigRootIntervals(ChainIntervalField::RestartGap, e))?,
            account_lifetime: ChainInterval::root(
                intervals.account_lifetime,
                intervals.account_lifetime_in_blocks,
            )
            .map_err(|e| Error::ConfigRootIntervals(ChainIntervalField::AccountLifetime, e))?,
        })
    }
}

pub struct ChainIntervals {
    pub restart_gap: ChainInterval,
    pub account_lifetime: ChainInterval,
}

impl ChainIntervals {
    pub fn new(root: RootChainIntervals, chain: &ArgChainIntervals) -> Result<Self, TaskError> {
        Ok(Self {
            restart_gap: ChainInterval::chain(root, chain.restart_gap, chain.restart_gap_in_blocks)
                .map_err(|e| TaskError::ChainInterval(ChainIntervalField::RestartGap, e))?,
            account_lifetime: ChainInterval::chain(
                root,
                chain.account_lifetime,
                chain.account_lifetime_in_blocks,
            )
            .map_err(|e| TaskError::ChainInterval(ChainIntervalField::AccountLifetime, e))?,
        })
    }
}

#[derive(Clone, Copy)]
pub enum ChainInterval {
    Time(Timestamp),
    Blocks(BlockNumber),
}

impl ChainInterval {
    fn new(
        time: Option<Timestamp>,
        blocks: Option<BlockNumber>,
    ) -> Result<Self, ChainIntervalError> {
        match (time, blocks) {
            (None, None) => Err(ChainIntervalError::NotSet),
            (Some(_), Some(_)) => Err(ChainIntervalError::DoubleSet),
            (None, Some(b)) => Ok(Self::Blocks(b)),
            (Some(t), None) => Ok(Self::Time(t)),
        }
    }

    fn chain(
        root: RootChainIntervals,
        time: Option<Timestamp>,
        blocks: Option<BlockNumber>,
    ) -> Result<Self, ChainIntervalError> {
        match ChainInterval::new(time, blocks) {
            Ok(ci) => Ok(ci),
            Err(error @ ChainIntervalError::NotSet) => root.restart_gap.ok_or(error),
            Err(error @ ChainIntervalError::DoubleSet) => Err(error),
        }
    }

    fn root(
        time: Option<Timestamp>,
        blocks: Option<BlockNumber>,
    ) -> Result<Option<Self>, ChainIntervalError> {
        match Self::new(time, blocks) {
            Ok(ci) => Ok(Some(ci)),
            Err(ChainIntervalError::NotSet) => Ok(None),
            Err(error @ ChainIntervalError::DoubleSet) => Err(error),
        }
    }
}

#[derive(Debug)]
pub enum ChainIntervalField {
    RestartGap,
    AccountLifetime,
}

impl Display for ChainIntervalField {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.backtick(|formatter| {
            formatter.write_str(match self {
                ChainIntervalField::RestartGap => "restart-gap",
                ChainIntervalField::AccountLifetime => "account-lifetime",
            })
        })
    }
}

fn decode_hex_for_visitor<T, E: DeError>(
    string: &str,
    decode: impl FnOnce(&str) -> Result<T, FromHexError>,
) -> Result<T, E> {
    if let Some(stripped) = string.strip_prefix(HEX_PREFIX) {
        decode(stripped).map_err(DeError::custom)
    } else {
        Err(DeError::invalid_value(
            Unexpected::Str(string),
            &"a string prefixed with \"0x\"",
        ))
    }
}

enum Quote {
    Double,
    Backtick,
}

impl From<Quote> for &'static str {
    fn from(quote: Quote) -> Self {
        match quote {
            Quote::Double => "\"",
            Quote::Backtick => "`",
        }
    }
}

pub trait EncaseInQuotes {
    fn encase_in(
        quote: &'static str,
        f: &mut Formatter<'_>,
        format: impl FnOnce(&mut Formatter<'_>) -> FmtResult,
    ) -> FmtResult {
        f.write_str(quote)?;

        format(f)?;

        f.write_str(quote)
    }

    fn double(&mut self, format: impl FnOnce(&mut Formatter<'_>) -> FmtResult) -> FmtResult;

    fn backtick(&mut self, format: impl FnOnce(&mut Formatter<'_>) -> FmtResult) -> FmtResult;
}

impl EncaseInQuotes for Formatter<'_> {
    fn double(&mut self, format: impl FnOnce(&mut Formatter<'_>) -> FmtResult) -> FmtResult {
        Self::encase_in("\"", self, format)
    }

    fn backtick(&mut self, format: impl FnOnce(&mut Formatter<'_>) -> FmtResult) -> FmtResult {
        Self::encase_in("`", self, format)
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Decimals(pub u8);

// pub struct Bytes(pub Vec<u8>);

// impl Deref for Bytes {
//     type Target = Vec<u8>;

//     fn deref(&self) -> &Self::Target {
//         &self.0
//     }
// }

// impl<'de> Deserialize<'de> for Bytes {
//     fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
//         struct BytesVisitor;

//         impl Visitor<'_> for BytesVisitor {
//             type Value = Bytes;

//             fn expecting(&self, f: &mut Formatter<'_>) -> FmtResult {
//                 write!(f, "a hexidecimal string prefixed with {HEX_PREFIX:?}")
//             }

//             fn visit_str<E: DeError>(self, string: &str) -> Result<Self::Value, E> {
//                 decode_hex_for_visitor(string, |stripped| const_hex::decode(stripped).map(Bytes))
//             }
//         }

//         deserializer.deserialize_str(BytesVisitor)
//     }
// }

// #[derive(Debug, Clone, Copy)]
// pub enum Pallet {
//     System,
// }

// impl Pallet {
//     pub fn to_str(self) -> &'static str {
//         match self {
//             Self::System => "System",
//         }
//     }
// }

// impl Display for Pallet {
//     fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
//         f.backtick(|formatter| formatter.write_str(self.to_str()))
//     }
// }

// #[derive(Debug, Clone, Copy)]
// pub enum Constant {
//     System(SystemConstant),
// }

// impl Constant {
//     pub fn pallet_n_constant(self) -> (Pallet, &'static str) {
//         match self {
//             Constant::System(constant) => (Pallet::System, constant.to_str()),
//         }
//     }
// }

// impl Display for Constant {
//     fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
//         let (pallet, constant) = self.pallet_n_constant();

//         f.write_str("the constant ")?;
//         f.backtick(|formatter| formatter.write_str(constant))?;
//         f.write_str("in the pallet ")?;

//         f.backtick(|formatter| formatter.write_str(pallet.to_str()))
//     }
// }

// #[derive(Debug, Clone, Copy)]
// pub enum SystemConstant {
//     Version,
//     SS58Prefix,
// }

// impl SystemConstant {
//     pub fn to_str(self) -> &'static str {
//         match self {
//             Self::Version => "Version",
//             Self::SS58Prefix => "SS58Prefix",
//         }
//     }
// }

// impl From<SystemConstant> for Constant {
//     fn from(constant: SystemConstant) -> Self {
//         Constant::System(constant)
//     }
// }
