// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use reqwest::{IntoUrl, Url};
use serde::{Deserialize, Serialize};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),

    #[error(transparent)]
    Url(#[from] url::ParseError),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RootResponse {}

#[derive(Debug)]
pub struct Client {
    client: reqwest::Client,
    base_url: Url,
}

impl Client {
    pub fn new<U: IntoUrl>(client: reqwest::Client, base_url: U) -> Result<Self> {
        Ok(Self {
            client,
            base_url: base_url.into_url()?,
        })
    }

    pub async fn root(&self) -> Result<RootResponse> {
        Ok(self
            .client
            .get(self.base_url.join("/")?)
            .send()
            .await?
            .json()
            .await?)
    }
}
