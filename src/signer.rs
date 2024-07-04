//! The signer module.
//!
//! Keep in mind that leaking secrets in a system like Kalatori is a serious threat with delayed
//! attacks taken into account. Of course, some secret rotation scheme must be implemented but it
//! seems likely that it would be neglected occasionally.
//!
//! Also this abstraction could be used to implement an off-system signer.

use crate::{
    arguments::{OLD_SEED, SEED},
    chain::definitions::H256,
    database::definitions::Public,
    error::SeedEnvError,
};
use ahash::{HashMap, RandomState};
use indexmap::IndexMap;
use mnemonic_external::{
    error::ErrorWordList, wordlist::WORDLIST_ENGLISH, AsWordList, Bits11, WordListElement, WordSet,
};
use std::{env, str};
use substrate_crypto_light::sr25519::Pair;
use tokio::sync::RwLock;
use tracing::Level;
use zeroize::Zeroize;

pub struct KeyStore {
    pair: (Public, Entropy),
    old_pairs: IndexMap<Public, (Entropy, String), RandomState>,
}

impl KeyStore {
    pub fn parse() -> Result<Self, SeedEnvError> {
        const SEED_BYTES: &[u8] = SEED.as_bytes();

        let mut pair_option = None;
        let mut old_pairs = IndexMap::default();

        for (raw_name, raw_value) in env::vars_os() {
            match raw_name.as_encoded_bytes() {
                SEED_BYTES => {
                    env::remove_var(raw_name);

                    pair_option = {
                        Some(Entropy::new_pair(
                            raw_value
                                .to_str()
                                .ok_or(SeedEnvError::InvalidUnicodeValue)?,
                        )?)
                    };
                }
                raw_name_bytes => {
                    // TODO: Use `OsStr::slice_encoded_bytes()` instead.
                    // https://github.com/rust-lang/rust/issues/118485
                    if let Some(stripped_raw_name) =
                        raw_name_bytes.strip_prefix(OLD_SEED.as_bytes())
                    {
                        env::remove_var(&raw_name);

                        let name = str::from_utf8(stripped_raw_name)?;
                        let seed = raw_value
                            .to_str()
                            .ok_or_else(|| SeedEnvError::InvalidUnicodeOldValue(name.into()))?;
                        let pair = Entropy::new_pair(seed)?;

                        tracing::debug!("Parsed `{OLD_SEED}{name}` ({:#}).", H256::from(pair.0));

                        old_pairs.insert(pair.0, (pair.1, name.into()));
                    }
                }
            }
        }

        Ok(Self {
            pair: pair_option.ok_or(SeedEnvError::NotSet)?,
            old_pairs,
        })
    }

    pub fn old_pairs_len(&self) -> usize {
        self.old_pairs.len()
    }

    pub fn remove(&mut self, key: &Public) -> Option<(Entropy, String)> {
        self.old_pairs.swap_remove(key)
    }

    pub fn into_signer<const IS_DB_OLD: bool>(
        self,
        filtered_old_pairs: HashMap<Public, (Entropy, String)>,
    ) -> Signer {
        if IS_DB_OLD {
            for (_, name) in self.old_pairs.into_values() {
                tracing::warn!(
                    "`{OLD_SEED}{name:?}` has no matching public keys and thus is ignored."
                );
            }
        } else {
            tracing::warn!(
                "The daemon has no existing database, so all `{OLD_SEED}*` are ignored."
            );
        }

        Signer {
            old_pairs: RwLock::new(filtered_old_pairs),
            pair: self.pair,
        }
    }

    pub fn public(&self) -> Public {
        self.pair.0
    }
}

pub struct Entropy(Vec<u8>);

impl Drop for Entropy {
    fn drop(&mut self) {
        if tracing::enabled!(Level::DEBUG) {
            let public = self.public();

            self.0.zeroize();

            tracing::debug!("Zeroized the key {:#}.", H256::from(public));
        } else {
            self.0.zeroize();
        }
    }
}

impl Entropy {
    fn new(seed: &str) -> Result<Self, SeedEnvError> {
        let mut word_set = WordSet::new();

        for word in seed.split_whitespace() {
            word_set.add_word(word, &WordList)?;
        }

        let entropy = word_set.to_entropy()?;

        Ok(Self(entropy))
    }

    fn new_pair(seed: &str) -> Result<(Public, Self), SeedEnvError> {
        Self::new(seed).map(|entropy| (entropy.public(), entropy))
    }

    fn public(&self) -> Public {
        Pair::from_entropy_and_pwd(&self.0, "")
            .unwrap()
            .public()
            .into()
    }
}

struct WordList;

impl AsWordList for WordList {
    type Word = &'static str;

    fn get_word(&self, bits: Bits11) -> Result<Self::Word, ErrorWordList> {
        let i: usize = bits.bits().into();

        Ok(WORDLIST_ENGLISH[i])
    }

    fn get_words_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<WordListElement<Self>>, ErrorWordList> {
        let Some(start) = WORDLIST_ENGLISH.iter().position(|w| w.starts_with(prefix)) else {
            return Ok(vec![]);
        };

        Ok(WORDLIST_ENGLISH[start..]
            .iter()
            .enumerate()
            .filter_map(|(i, w)| {
                w.starts_with(prefix).then_some(WordListElement {
                    word: *w,
                    bits11: bits11_from_index(i),
                })
            })
            .collect())
    }

    fn bits11_for_word(&self, word: &str) -> Result<Bits11, ErrorWordList> {
        WORDLIST_ENGLISH
            .iter()
            .position(|w| *w == word)
            .map(bits11_from_index)
            .ok_or(ErrorWordList::NoWord)
    }
}

// Used only where `i` will never cause a panic here.
fn bits11_from_index(i: usize) -> Bits11 {
    Bits11::from(i.try_into().unwrap()).unwrap()
}

pub struct Signer {
    pair: (Public, Entropy),
    old_pairs: RwLock<HashMap<Public, (Entropy, String)>>,
}
