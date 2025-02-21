// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

#[derive(Debug, Clone, clap::Parser)]
#[group(id = "fetcher")]
#[command(next_help_heading = "Fetcher")]
pub struct FetcherArgs {
    /// Destination path.
    #[arg(long, short)]
    pub out: PathBuf,
}

impl FetcherArgs {
    pub fn iter(&self) -> impl Iterator<Item = OsString> {
        [Some(flag_val("out", &self.out))].into_iter().flatten()
    }
}

fn flag_val(name: &str, value: impl AsRef<OsStr>) -> OsString {
    let mut oss = OsString::with_capacity(2 + name.len() + 1 + value.as_ref().len());
    oss.push("--");
    oss.push(name);
    oss.push("=");
    oss.push(value);
    oss
}
