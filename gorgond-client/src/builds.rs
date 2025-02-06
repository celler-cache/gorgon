// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use crate::{Client, Request, Result};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use url::Url;
use uuid::Uuid;

pub struct BuildsClient<'c>(pub &'c Client);
impl BuildsClient<'_> {
    pub async fn allocate<'a>(
        &self,
        builder: impl Into<Cow<'_, str>>,
    ) -> Result<BuildAllocateResponse<'static>> {
        BuildAllocateRequest {
            builder: builder.into(),
        }
        .send(self.0)
        .await
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Build<'a> {
    pub id: Uuid,
    pub builder: Cow<'a, str>,
    pub source: Url,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BuildAllocateRequest<'a> {
    pub builder: Cow<'a, str>,
}
impl Request for BuildAllocateRequest<'_> {
    type Response = BuildAllocateResponse<'static>;
    fn endpoint(&self, base: &Url) -> Result<(Method, Url), url::ParseError> {
        Ok((Method::PUT, base.join("builds/allocate")?))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BuildAllocateResponse<'a> {
    pub build: Build<'a>,
}
