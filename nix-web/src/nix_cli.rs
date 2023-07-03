// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use tokio::process::Command;
use tracing::{debug, debug_span, instrument, warn};

/// Path to the nix binary. Default set in: .cargo/config.toml
pub const NIX_CLI_PATH: &str = env!("NIX_WEB_BUILD_NIX_CLI_PATH");

#[instrument(name = "nix", skip_all)]
async fn exec(cmd: &mut Command) -> Result<Vec<u8>> {
    let cmd = cmd.stdin(std::process::Stdio::null());
    debug!(?cmd, "Calling nix...");

    let out = cmd.output().await?;
    if !out.status.success() {
        let last_line = debug_span!("stderr").in_scope(|| {
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .inspect(|line| warn!("{}", line))
                .last()
                .map(|s| s.strip_prefix("error:").unwrap_or(s).trim().to_string())
        });
        return Err(Error::NixCommandError(
            out.status.code().unwrap_or(0),
            last_line.unwrap_or("".into()),
        ));
    }
    Ok(out.stdout)
}

/// Returns logs for a derivation.
pub async fn log<P: AsRef<OsStr>>(path: P) -> Result<String> {
    Ok(String::from_utf8_lossy(
        &exec(
            Command::new(NIX_CLI_PATH)
                .arg("--experimental-features")
                .arg("nix-command")
                .arg("log")
                .arg(path),
        )
        .await?[..],
    )
    .into())
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Derivation {
    #[serde(default, skip_serializing, skip_deserializing)]
    pub path: String, // Filled in from the map key.
    #[serde(default, skip_serializing, skip_deserializing)]
    pub status: DerivationStatus,

    // From `nix show-derivation`.
    pub args: Vec<String>,
    pub builder: String,
    pub env: BTreeMap<String, String>,
    #[serde(rename = "inputDrvs")]
    pub input_drvs: BTreeMap<String, Vec<String>>,
    #[serde(rename = "inputSrcs")]
    pub input_srcs: Vec<String>,
    pub outputs: BTreeMap<String, DerivationOutput>,
    pub system: String,
}

impl Derivation {
    pub async fn get<
        AI: IntoIterator<Item = A>,
        A: AsRef<OsStr>,
        PI: IntoIterator<Item = PS>,
        PS: AsRef<OsStr>,
    >(
        args: AI,
        paths: PI,
    ) -> Result<BTreeMap<String, Self>> {
        Ok(serde_json::from_slice(
            &exec(
                Command::new(NIX_CLI_PATH)
                    .arg("--experimental-features")
                    .arg("nix-command")
                    .arg("show-derivation")
                    .arg("-r")
                    .args(args)
                    .args(paths),
            )
            .await?[..],
        )?)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum DerivationStatus {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "miss")]
    Missing,
    #[serde(rename = "succ")]
    Success,
}

impl DerivationStatus {
    pub fn is_succ(&self) -> bool {
        self == &Self::Success
    }
}

impl Default for DerivationStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

impl std::fmt::Display for DerivationStatus {
    fn fmt<'w>(&self, w: &mut std::fmt::Formatter<'w>) -> std::fmt::Result {
        match self {
            Self::Unknown => write!(w, "unknown"),
            Self::Missing => write!(w, "miss"),
            Self::Success => write!(w, "succ"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DerivationOutput {
    pub path: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PathInfo {
    pub path: String,
    // Only present (as the only item alongside path) for invalid paths.
    #[serde(default = "PathInfo::default_valid")]
    pub valid: bool,

    pub deriver: Option<String>,
    pub ca: Option<String>,
    #[serde(rename = "narHash")]
    pub nar_hash: Option<String>,
    #[serde(rename = "narSize")]
    pub nar_size: Option<usize>,
    pub references: Option<Vec<String>>,
    #[serde(rename = "registrationTime")]
    pub registration_time: Option<u64>, // TODO: Parse timestamp.
    pub signatures: Option<Vec<String>>,
}

impl PathInfo {
    pub async fn get<
        AI: IntoIterator<Item = A>,
        A: AsRef<OsStr>,
        PI: IntoIterator<Item = PS>,
        PS: AsRef<OsStr>,
    >(
        args: AI,
        paths: PI,
    ) -> Result<BTreeMap<String, Self>> {
        Ok(serde_json::from_slice::<Vec<PathInfo>>(
            &exec(
                Command::new(NIX_CLI_PATH)
                    .arg("--experimental-features")
                    .arg("nix-command")
                    .arg("path-info")
                    .arg("--json")
                    .args(args)
                    .args(paths),
            )
            .await?[..],
        )?
        .drain(..)
        .filter(|pi| pi.valid)
        .map(|pi| (pi.path.clone(), pi))
        .collect())
    }

    fn default_valid() -> bool {
        true
    }
}
