// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{str::FromStr, sync::LazyLock};

use directories::ProjectDirs;

pub mod tasks;

pub static DIRS: LazyLock<ProjectDirs> = LazyLock::new(|| {
    ProjectDirs::from("org", "Gorgon", "gorgond").expect("Couldn't find project directories!?")
});

pub fn parse_duration(s: &str) -> Result<chrono::Duration, clap::Error> {
    humantime::Duration::from_str(s)
        .map_err(|err| clap::Error::raw(clap::error::ErrorKind::InvalidValue, err))
        .and_then(|d| {
            chrono::Duration::from_std(*d)
                .map_err(|err| clap::Error::raw(clap::error::ErrorKind::InvalidValue, err))
        })
}
