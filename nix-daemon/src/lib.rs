// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

pub mod wire;

use chrono::{DateTime, Utc};
use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

trait ResultExt<T, E> {
    fn with_field(self, f: &'static str) -> Result<T>;
}

impl<T, E: Into<Error>> ResultExt<T, E> for Result<T, E> {
    fn with_field(self, f: &'static str) -> Result<T> {
        self.map_err(|err| Error::Field(f, Box::new(err.into())))
    }
}

#[derive(Debug, Error)]
pub enum Error {
    /// This error was encountered while reading/writing a specific field.
    #[error("`{0}`: {1}")]
    Field(&'static str, #[source] Box<Error>),
    #[error("invalid value: {0}")]
    Invalid(String),

    #[error(transparent)]
    IO(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u8, u8);

impl Version {
    fn since(&self, v: u8) -> bool {
        self.1 >= v
    }
}

/// Data about a Nix store path.
#[derive(Debug, PartialEq, Eq)]
pub struct PathInfo {
    /// Derivation that produced this path.
    pub deriver: Option<String>,
    /// Paths referenced by this path.
    pub references: Vec<String>,

    /// NAR hash (in the form: [algo]-[hash]).
    pub nar_hash: String,
    /// NAR size.
    pub nar_size: u64,

    /// Is this path "ultimately trusted", eg. built locally?
    pub ultimate: bool,
    /// Optional signatures, eg. from a binary cache.
    pub signatures: Vec<String>,
    /// An assertion that this path is content-addressed, eg. for fixed-output derivations.
    pub ca: Option<String>,

    /// When the path was registered, eg. placed into the local store.
    pub registration_time: DateTime<Utc>,
}
