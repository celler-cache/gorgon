// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use crate::Result;
use chrono::prelude::*;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Build {
    pub name: String,
    pub machine_name: String,
    pub start: DateTime<Utc>,
    pub start_phase: Vec<(String, DateTime<Utc>)>,
    pub end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetDerivationBuildsResponse {
    pub builds: Vec<Build>,
}

impl Build {
    pub async fn list(base: &str, path: &str) -> Result<GetDerivationBuildsResponse> {
        Ok(reqwest::get(format!("{}{}/builds", base, path))
            .await?
            .json::<GetDerivationBuildsResponse>()
            .await?)
    }
}
