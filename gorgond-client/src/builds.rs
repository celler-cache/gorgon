// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use crate::{Client, Request, Result, Yoke, Yokeable};
use chrono::prelude::*;
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
    ) -> Result<Yoke<BuildAllocateResponse<'static>>> {
        BuildAllocateRequest {
            builder: builder.into(),
        }
        .send(self.0)
        .await
    }
}

#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct Build<'a> {
    pub id: Uuid,
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    #[serde(borrow)]
    pub kind: Cow<'a, str>,
    #[serde(borrow)]
    pub source: Cow<'a, str>,
}

/// A task performed as part of a Build.
#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct Task<'a> {
    /// ID of the parent Build.
    pub build_id: Uuid,
    /// Sub-ID of this task. Unique together with build_id, but may be reused between builds.
    /// This may be a UUIDv8 with a build system specific ID unceremoniously shoved into it.
    pub task_id: Uuid,
    /// Sub-ID of the parent task, if any. The parent task must have the same build_id.
    pub parent_task_id: Uuid,

    #[serde(borrow)]
    pub name: Cow<'a, str>,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct BuildAllocateRequest<'a> {
    #[serde(borrow)]
    pub builder: Cow<'a, str>,
}
impl Request for BuildAllocateRequest<'_> {
    type Response = BuildAllocateResponse<'static>;
    fn endpoint(&self, base: &Url) -> Result<(Method, Url), url::ParseError> {
        Ok((Method::PUT, base.join("builds/allocate")?))
    }
}

#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct BuildAllocateResponse<'a> {
    #[serde(borrow)]
    pub build: Build<'a>,
}
