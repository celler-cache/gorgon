// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use derive_more::Unwrap;
use miette::Diagnostic;
use reqwest::IntoUrl;
use std::{borrow::Cow, fmt::Debug};

pub use url::Url;
pub use uuid::Uuid;
pub use yoke::Yokeable;

pub mod api;
pub mod models;

mod request;
pub use crate::request::*;

pub type Yoke<T, C = String> = yoke::Yoke<T, C>;

#[derive(Debug, thiserror::Error, Diagnostic, Unwrap)]
pub enum ClientError {
    #[error("Invalid Base URL")]
    BaseUrl(#[source] reqwest::Error),
}

#[derive(Debug)]
pub struct Client {
    client: reqwest::Client,
    base_url: Url,
}

impl Client {
    pub fn new<U: IntoUrl>(base_url: U) -> Result<Self, ClientError> {
        Self::new_with(reqwest::Client::new(), base_url)
    }

    pub fn new_with<U: IntoUrl>(client: reqwest::Client, base_url: U) -> Result<Self, ClientError> {
        Ok(Self {
            client,
            base_url: base_url.into_url().map_err(ClientError::BaseUrl)?,
        })
    }

    pub async fn root(&self) -> Result<Yoke<api::RootResponse>, RequestError> {
        api::RootRequest {}.send(self).await
    }

    pub fn builds(&self) -> BuildsClient {
        BuildsClient(self)
    }
}

pub struct BuildsClient<'c>(pub &'c Client);
impl BuildsClient<'_> {
    pub async fn allocate<'a>(
        &self,
        builder: impl Into<Cow<'_, str>>,
    ) -> Result<Yoke<api::BuildAllocateResponse<'static>>, RequestError> {
        api::BuildAllocateRequest {
            builder: builder.into(),
        }
        .send(self.0)
        .await
    }
}
