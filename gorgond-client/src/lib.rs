// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like our Error enum, and puts a warning on *everything*
// returning one if it's not silenced like this.
#![allow(clippy::result_large_err)]

use derive_more::{Constructor, Unwrap};
use miette::{Diagnostic, NamedSource, SourceOffset, SourceSpan};
use reqwest::{IntoUrl, Method, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt::{Debug, Display};
use std::future::Future;
use strum::{AsRefStr, IntoStaticStr};

pub use uuid::Uuid;

pub mod builds;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, Diagnostic, Unwrap)]
pub enum Error {
    #[error("Invalid Base URL")]
    BaseUrl(#[source] reqwest::Error),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Request(#[from] RequestError),
}

#[derive(Debug, thiserror::Error, Diagnostic, Constructor)]
#[diagnostic(forward(kind))]
pub struct RequestError {
    pub endpoint: Option<(Method, Url)>,
    #[source]
    pub kind: RequestErrorKind,
}
impl Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.endpoint {
            Some((method, url)) => write!(f, "{} {}", method, url),
            None => write!(f, "Invalid endpoint"),
        }
    }
}

#[derive(Debug, thiserror::Error, Diagnostic, Unwrap)]
pub enum RequestErrorKind {
    #[error("Bad URL")]
    Url(#[from] url::ParseError),
    #[error("Request Failed")]
    Request(#[from] reqwest::Error),
    #[error("HTTP {}: {}", .0.status, .0.status_reason())]
    #[diagnostic(transparent)]
    Api(#[from] ApiError),
    #[error("JSON Error")]
    Json(
        #[label] SourceSpan,
        #[source_code] NamedSource<String>,
        #[source] serde_json::Error,
    ),
}

pub trait Request: Serialize + Debug {
    type Response: DeserializeOwned;
    fn endpoint(&self, base: &Url) -> Result<(Method, Url), url::ParseError>;

    fn send(&self, client: &Client) -> impl Future<Output = Result<Self::Response, Error>> + Send {
        // Serialize self outside the future, this avoids needing to send &Self between threads.
        let endpoint_r = self.endpoint(&client.base_url);
        let body_r = serde_json::to_string_pretty(&self).map_err(|err| {
            let src = format!("{self:#?}");
            RequestErrorKind::Json(
                SourceSpan::new(0.into(), src.len()),
                NamedSource::new(std::any::type_name_of_val(self), src),
                err,
            )
        });

        // Then go async.
        async move {
            let (method, url) = endpoint_r.map_err(|err| RequestError::new(None, err.into()))?;

            // Shut up, clippy. This closure exists so that the inner one can return a
            // RequestErrorKind, and the outer one can draw the rest of the RequestError.
            #[allow(clippy::redundant_closure_call)]
            (async |method: &Method, url: &Url| -> Result<Self::Response, RequestErrorKind> {
                let req = client
                    .client
                    .request(method.clone(), url.clone())
                    .body(body_r?)
                    .build()?;
                let rsp = client.client.execute(req).await?;
                let status = rsp.status();
                let body = rsp.text().await?;

                fn parse<T: DeserializeOwned>(
                    url: &Url,
                    body: String,
                ) -> Result<T, RequestErrorKind> {
                    serde_json::from_str(&body).map_err(|err| {
                        RequestErrorKind::Json(
                            SourceSpan::new(
                                SourceOffset::from_location(&body, err.line(), err.column()),
                                body.lines()
                                    .nth(err.line() - 1)
                                    .map(|line| line.len() - err.column())
                                    .unwrap_or(0),
                            ),
                            NamedSource::new(url, body).with_language("JSON"),
                            err,
                        )
                    })
                }
                match status {
                    StatusCode::OK => Ok(parse(url, body)?),
                    _ => Err(RequestErrorKind::Api(parse(url, body)?)),
                }
            })(&method, &url)
            .await
            .map_err(|err| RequestError::new(Some((method, url)), err).into())
        }
    }
}

/// Wraps an ApiError in RFC 9457 ("Problem Details for HTTP APIs") format:
/// https://www.rfc-editor.org/rfc/rfc9457.html
#[derive(Debug, thiserror::Error, Diagnostic, Serialize, Deserialize)]
#[diagnostic(forward(error))]
#[error("{detail}")]
pub struct ApiError {
    pub status: u16,
    pub title: Cow<'static, str>,
    pub detail: String,
    #[serde(flatten)]
    pub error: ApiErrorKind,
}
impl ApiError {
    pub fn status_reason(&self) -> &str {
        StatusCode::from_u16(self.status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("[INVALID]")
    }
}
impl From<ApiErrorKind> for ApiError {
    fn from(err: ApiErrorKind) -> Self {
        Self {
            status: err.status().as_u16(),
            title: Cow::Borrowed((&err).into()),
            detail: err.to_string(),
            error: err,
        }
    }
}
impl From<ApiError> for ApiErrorKind {
    fn from(err: ApiError) -> Self {
        err.error
    }
}

#[derive(Debug, thiserror::Error, Diagnostic, Serialize, Deserialize, AsRefStr, IntoStaticStr)]
#[serde(tag = "type")]
#[strum(serialize_all = "title_case")]
pub enum ApiErrorKind {
    #[error("No such route: {route}")]
    #[serde(rename = "tag:NoSuchRoute")]
    NoSuchRoute { route: String },

    #[error("unknown error type: {0}")]
    #[serde(untagged)]
    Unknown(String),
}

impl ApiErrorKind {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NoSuchRoute { .. } => StatusCode::NOT_FOUND,
            Self::Unknown(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[cfg(feature = "axum")]
impl axum::response::IntoResponse for ApiErrorKind {
    fn into_response(self) -> axum::response::Response {
        use axum_extra::response::ErasedJson;
        (self.status(), ErasedJson::pretty(ApiError::from(self))).into_response()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RootRequest {}

impl Request for RootRequest {
    type Response = RootResponse;
    fn endpoint(&self, base: &Url) -> Result<(Method, Url), url::ParseError> {
        Ok((Method::GET, base.join("")?))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RootResponse {}

#[derive(Debug)]
pub struct Client {
    client: reqwest::Client,
    base_url: Url,
}

impl Client {
    pub fn new<U: IntoUrl>(base_url: U) -> Result<Self> {
        Self::new_with(reqwest::Client::new(), base_url)
    }

    pub fn new_with<U: IntoUrl>(client: reqwest::Client, base_url: U) -> Result<Self> {
        Ok(Self {
            client,
            base_url: base_url.into_url().map_err(Error::BaseUrl)?,
        })
    }

    pub async fn root(&self) -> Result<RootResponse> {
        RootRequest {}.send(self).await
    }

    pub fn builds(&self) -> builds::BuildsClient {
        builds::BuildsClient(self)
    }
}
