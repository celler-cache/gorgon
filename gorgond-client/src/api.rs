// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::borrow::Cow;

use miette::Diagnostic;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use strum::{AsRefStr, IntoStaticStr};
use url::Url;
use yoke::Yokeable;

use crate::{mkrequest, models};

/// Wraps an ApiError in RFC 9457 ("Problem Details for HTTP APIs") format:
/// https://www.rfc-editor.org/rfc/rfc9457.html
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiErrorWrapper {
    pub status: u16,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub detail: Option<String>,
    #[serde(flatten)]
    pub inner: ApiError,
}
impl From<ApiError> for ApiErrorWrapper {
    fn from(err: ApiError) -> Self {
        Self {
            status: err.status().as_u16(),
            title: err.to_string(),
            detail: err.help().map(|help| help.to_string()),
            inner: err,
        }
    }
}
impl From<ApiErrorWrapper> for ApiError {
    fn from(wrap: ApiErrorWrapper) -> Self {
        wrap.inner
    }
}

#[derive(Debug, thiserror::Error, Diagnostic, Serialize, Deserialize, AsRefStr, IntoStaticStr)]
#[serde(tag = "type")]
#[strum(serialize_all = "title_case")]
pub enum ApiError {
    #[error("No such route: {route}")]
    #[serde(rename = "tag:NoSuchRoute")]
    NoSuchRoute { route: String },

    #[error("Method not allowed: {method} {route}")]
    #[serde(rename = "tag:MethodNotAllowed")]
    MethodNotAllowed { method: String, route: String },

    #[error("Bad request body")]
    #[serde(rename = "tag:BadRequestBody")]
    BadRequestBody { error: String },
    #[error("Bad request JSON")]
    #[serde(rename = "tag:BadRequestJson")]
    BadRequestJson { error: String },

    #[error("unknown error type: {0}")]
    #[serde(untagged)]
    Unknown(String),
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NoSuchRoute { .. } => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed { .. } => StatusCode::METHOD_NOT_ALLOWED,
            Self::BadRequestBody { .. } | Self::BadRequestJson { .. } => StatusCode::BAD_REQUEST,
            Self::Unknown(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

mkrequest!("", RootRequest, RootResponse);
#[derive(Debug, Default, Serialize, Deserialize, Yokeable)]
pub struct RootRequest {}
#[derive(Debug, Default, Serialize, Deserialize, Yokeable)]
pub struct RootResponse {}

mkrequest!(
    "/builds/allocate",
    BuildAllocateRequest<'_>,
    BuildAllocateResponse<'static>
);
#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct BuildAllocateRequest<'a> {
    #[serde(borrow)]
    pub builder: Cow<'a, str>,
}
#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct BuildAllocateResponse<'a> {
    #[serde(borrow)]
    pub build: models::Build<'a>,
}
