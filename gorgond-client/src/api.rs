// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use derive_more::Constructor;
use miette::Diagnostic;
use ownable::{IntoOwned, ToOwned};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use strum::{AsRefStr, IntoStaticStr};
use yoke::Yokeable;

use crate::{models, Client, Request, Result, Yoke};

/// An error returned from the API (wrapped in an ApiErrorWrapper).
#[derive(Debug, thiserror::Error, Diagnostic, Serialize, Deserialize, AsRefStr, IntoStaticStr)]
#[serde(tag = "type")]
#[strum(serialize_all = "title_case")]
pub enum ApiError {
    #[error("No such route: {route}")]
    #[serde(rename = "tag:NoSuchRoute")]
    NoSuchRoute { method: String, route: String },

    #[error("Method not allowed: {method} {route}")]
    #[serde(rename = "tag:MethodNotAllowed")]
    MethodNotAllowed { method: String, route: String },

    #[error("Bad request body")]
    #[serde(rename = "tag:BadRequestBody")]
    BadRequestBody { error: String },
    #[error("Bad request JSON")]
    #[serde(rename = "tag:BadRequestJson")]
    BadRequestJson { error: String },

    #[error("Not found in database")]
    #[serde(rename = "tag:DatabaseNotFound")]
    DatabaseNotFound { error: String },
    #[error("Attempted to insert duplicate record")]
    #[serde(rename = "tag:DatabaseDuplicate")]
    DatabaseDuplicate { error: String },
    #[error("Database constraints violated")]
    #[serde(rename = "tag:DatabaseConstraints")]
    DatabaseConstraints { error: String },
    #[error("Database connection error")]
    #[serde(rename = "tag:DatabaseConnection")]
    DatabaseConnection { error: String },
    #[error("Database query failed")]
    #[serde(rename = "tag:DatabaseQuery")]
    DatabaseQueryError { error: String },

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
            Self::DatabaseNotFound { .. } => StatusCode::NOT_FOUND,
            Self::DatabaseDuplicate { .. } | Self::DatabaseConstraints { .. } => {
                StatusCode::BAD_REQUEST
            }
            Self::DatabaseConnection { .. } | Self::DatabaseQueryError { .. } => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Unknown(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

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

// The block is only included here for readability. If it starts causing problems, just yeet it and
// just use the macro to implement Request, like it did before I got fancy with it.
macro_rules! mkrequest {
    ($req:ty, $rsp:ty, { $($body:tt)* }) => {
        $($body)*
        impl $crate::Request for $req {
            type Response = $rsp;
        }
    };
}
macro_rules! mkmodelrequest {
    ($field:ident, $req:ident, $rsp:ident<$rspmodel:ident>) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
        pub struct $req {}
        #[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
        pub struct $rsp<'a> {
            #[serde(borrow)]
            pub $field: models::$rspmodel<'a>,
        }
        impl $crate::Request for $req {
            type Response = $rsp<'static>;
        }
    };
    ($field:ident, $req:ident<$reqmodel:ident>, $rsp:ident<$rspmodel:ident>) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
        pub struct $req<'a> {
            #[serde(borrow)]
            pub $field: models::$reqmodel<'a>,
        }
        #[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
        pub struct $rsp<'a> {
            #[serde(borrow)]
            pub $field: models::$rspmodel<'a>,
        }
        impl<'a> $crate::Request for $req<'a> {
            type Response = $rsp<'static>;
        }
    };
}

macro_rules! gen_endpoints {
    ($path:expr, ($($arg:ident),*), { fn $($ident:ident($req:ty) -> ($method:ident, $endpoint:literal);),* }) => {
        fn _path(&self, endpoint: &str) -> String {
            match endpoint {
                "" => format!($path, $($arg = self.$arg),*),
                _ => format!("{}/{endpoint}", format_args!($path, $($arg = self.$arg),*)),
            }
        }

        $(pub async fn $ident(&self, req: $req) -> Result<Yoke<<$req as Request>::Response>> {
            self.client.call(Method::$method, self._path($endpoint), req).await
        })*
    };
}

mkrequest!(RootRequest, RootResponse, {
    #[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
    pub struct RootRequest {}
    #[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
    pub struct RootResponse {}
});

mkmodelrequest!(project, ProjectGetRequest, ProjectGetResponse<Project>);
mkmodelrequest!(
    project,
    ProjectCreateRequest<ProjectSpec>,
    ProjectCreateResponse<Project>
);

#[derive(Debug, Constructor)]
pub struct ProjectsClient<'a> {
    client: &'a Client,
}
impl<'a> ProjectsClient<'a> {
    pub fn id(&self, id: &'a str) -> ProjectClient<'a> {
        ProjectClient::new(self.client, id)
    }
    gen_endpoints!("projects", (), {
        fn create(ProjectCreateRequest<'_>) -> (POST, "");
    });
}

#[derive(Debug, Constructor)]
pub struct ProjectClient<'a> {
    client: &'a Client,
    project_id: &'a str,
}
impl<'a> ProjectClient<'a> {
    pub fn builds(&self) -> ProjectsBuildsClient<'a> {
        ProjectsBuildsClient::new(self.client, self.project_id)
    }
    gen_endpoints!("projects/{project_id}", (project_id), {
        fn get(ProjectGetRequest) -> (GET, "");
    });
}

mkmodelrequest!(
    build,
    ProjectBuildCreateRequest<BuildSpec>,
    ProjectBuildCreateResponse<Build>
);

#[derive(Debug, Constructor)]
pub struct ProjectsBuildsClient<'a> {
    client: &'a Client,
    project_id: &'a str,
}
impl ProjectsBuildsClient<'_> {
    gen_endpoints!("projects/{project_id}/builds", (project_id), {
        fn create(ProjectBuildCreateRequest<'_>) -> (POST, "");
    });
}

#[derive(Debug, Constructor)]
pub struct WorkersClient<'a> {
    client: &'a Client,
}
impl<'a> WorkersClient<'a> {
    pub fn id(&self, id: &'a str) -> WorkerClient<'a> {
        WorkerClient::new(self.client, id)
    }
}

#[derive(Debug, Constructor)]
pub struct WorkerClient<'a> {
    client: &'a Client,
    id: &'a str,
}
impl WorkerClient<'_> {
    pub fn internal(&self) -> WorkerInternalClient<'_> {
        WorkerInternalClient::new(self.client, self.id)
    }
}

mkrequest!(
    WorkerInternalAllocateBuildRequest,
    WorkerInternalAllocateBuildResponse<'static>,
    {
        #[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
        pub struct WorkerInternalAllocateBuildRequest {}
        #[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
        pub struct WorkerInternalAllocateBuildResponse<'a> {
            #[serde(borrow)]
            pub build: models::Build<'a>,
        }
    }
);

#[derive(Debug, Constructor)]
pub struct WorkerInternalClient<'a> {
    client: &'a Client,
    id: &'a str,
}
impl WorkerInternalClient<'_> {
    gen_endpoints!("workers/{id}/internal", (id), {
        fn allocate_build(WorkerInternalAllocateBuildRequest) -> (PUT, "allocate-build");
    });
}
