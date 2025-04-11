// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like our Error enum, and puts a warning on *everything*
// returning one if it's not silenced like this.
#![allow(clippy::result_large_err)]

use derive_more::{Constructor, Unwrap};
use miette::{Diagnostic, MietteDiagnostic, NamedSource, SourceOffset, SourceSpan};
use reqwest::{header, IntoUrl, Method, StatusCode};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt::{Debug, Display};
use std::future::Future;
use strum::{AsRefStr, IntoStaticStr};

pub use url::Url;
pub use uuid::Uuid;
pub use yoke::Yokeable;

pub mod builds;

pub type Result<T, E = Error> = std::result::Result<T, E>;
pub type Yoke<T, C = String> = yoke::Yoke<T, C>;

#[derive(Debug, thiserror::Error, Diagnostic, Unwrap)]
#[allow(clippy::large_enum_variant)]
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
    type Response: for<'y> Yokeable<'y>;

    fn endpoint(&self, base: &Url) -> Result<(Method, Url), url::ParseError>;

    fn send(
        &self,
        client: &Client,
    ) -> impl Future<Output = Result<Yoke<Self::Response>, Error>> + Send
    where
        for<'y> <Self::Response as Yokeable<'y>>::Output: Deserialize<'y>,
    {
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
            (async |method: &Method, url: &Url| -> Result<Yoke<Self::Response>, RequestErrorKind> {
                let req = client
                    .client
                    .request(method.clone(), url.clone())
                    .body(body_r?)
                    .header(header::CONTENT_TYPE, "application/json")
                    .build()?;
                let rsp = client.client.execute(req).await?;
                let status = rsp.status();
                let body = rsp.text().await?;

                fn parse<'de, T: Deserialize<'de>>(
                    url: &Url,
                    body: &'de str,
                ) -> Result<T, RequestErrorKind> {
                    serde_json::from_str(body).map_err(|err| {
                        RequestErrorKind::Json(
                            SourceSpan::new(
                                SourceOffset::from_location(body, err.line(), err.column()),
                                body.lines()
                                    .nth(err.line() - 1)
                                    .map(|line| line.len() - err.column())
                                    .unwrap_or(0),
                            ),
                            NamedSource::new(url, body.to_string()).with_language("JSON"),
                            err,
                        )
                    })
                }
                match status {
                    StatusCode::OK => Yoke::try_attach_to_cart(body, |body| parse(url, body)),
                    _ => Err(RequestErrorKind::Api(parse(url, &body)?)),
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
#[diagnostic(forward(kind))]
#[error("{detail}")]
pub struct ApiError {
    pub status: u16,
    pub title: Cow<'static, str>,
    pub detail: String,
    #[serde(flatten)]
    pub kind: ApiErrorKind,
    #[source]
    #[diagnostic(transparent)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<MietteDiagnostic>,
}
impl ApiError {
    pub fn new<D: Diagnostic>(kind: ApiErrorKind, source: D) -> Self {
        Self::new_with(kind, Some(source))
    }
    pub fn new_err<E: Display>(kind: ApiErrorKind, source: E) -> Self {
        Self::new(kind, miette::MietteDiagnostic::new(source.to_string()))
    }
    pub fn new_with<D: Diagnostic>(kind: ApiErrorKind, source: Option<D>) -> Self {
        // Use the error message from the source error if given, else the error kind.
        let detail = source
            .as_ref()
            .map(|src| src.to_string())
            .unwrap_or_else(|| kind.to_string());

        // If we have a source, turn it into something serialisable
        let source = source
            .as_ref()
            .map(|src| MietteDiagnostic {
                message: src.to_string(),
                code: src.code().map(|v| v.to_string()),
                severity: src.severity(),
                help: src.help().map(|v| v.to_string()),
                url: src.url().map(|v| v.to_string()),
                labels: src.labels().map(|v| v.collect()),
            })
            // But skip it if there's no new information in it.
            .filter(|src| {
                src.message != detail
                    || src.code.is_some()
                    || src.severity.is_some()
                    || src.help.is_some()
                    || src.url.is_some()
                    || src.labels.is_some()
            });

        Self {
            status: kind.status().as_u16(),
            title: Cow::Borrowed((&kind).into()),
            detail,
            kind,
            source,
        }
    }

    pub fn with_status(mut self, status: impl Into<u16>) -> Self {
        self.status = status.into();
        self
    }
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn status_reason(&self) -> &str {
        StatusCode::from_u16(self.status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("[INVALID]")
    }
}
impl From<ApiErrorKind> for ApiError {
    fn from(kind: ApiErrorKind) -> Self {
        Self::new_with::<std::convert::Infallible>(kind, None)
    }
}

#[derive(Debug, thiserror::Error, Diagnostic, Serialize, Deserialize, AsRefStr, IntoStaticStr)]
#[serde(tag = "type")]
#[strum(serialize_all = "title_case")]
pub enum ApiErrorKind {
    #[error("No such route: {route}")]
    #[serde(rename = "tag:NoSuchRoute")]
    NoSuchRoute { route: String },

    #[error("Method not allowed: {method} {route}")]
    #[serde(rename = "tag:MethodNotAllowed")]
    MethodNotAllowed { method: String, route: String },

    #[error("Request rejected")]
    #[serde(rename = "tag:RequestRejected")]
    RequestRejected,

    #[error("unknown error type: {0}")]
    #[serde(untagged)]
    Unknown(String),
}

impl ApiErrorKind {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NoSuchRoute { .. } => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Yokeable)]
pub struct RootRequest {}

impl Request for RootRequest {
    type Response = RootResponse;
    fn endpoint(&self, base: &Url) -> Result<(Method, Url), url::ParseError> {
        Ok((Method::GET, base.join("")?))
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Yokeable)]
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

    pub async fn root(&self) -> Result<Yoke<RootResponse>> {
        RootRequest {}.send(self).await
    }

    pub fn builds(&self) -> builds::BuildsClient {
        builds::BuildsClient(self)
    }
}
