// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
};

use axum::{
    extract::{FromRequest, Path, State},
    response::IntoResponse,
    routing::{get, post, put},
    Router,
};
use axum_extra::response::ErasedJson;
use chrono::prelude::*;
use diesel::prelude::*;
use diesel_async::{
    pooled_connection::deadpool::Pool, AsyncConnection, AsyncPgConnection, RunQueryDsl,
};
use futures::stream::TryStreamExt;
use gorgond_client::{
    api::{self, ApiError, OwnableMap},
    models as client, Url, Uuid, Yoke, Yokeable,
};
use gorgond_diesel::{models, schema};
use miette::{Context, Diagnostic, IntoDiagnostic};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, info_span, instrument, warn, Span};
use tracing_indicatif::span_ext::IndicatifSpanExt;

pub type Result<T, E = ApiErrorAdapter> = std::result::Result<T, E>;

macro_rules! excluded {
    ($col:expr) => {
        $col.eq(diesel::upsert::excluded($col))
    };
    ($($col:expr),+$(,)?) => {
        ($($col.eq(diesel::upsert::excluded($col))),+)
    };
}

#[derive(Debug, Clone, clap::Parser)]
#[clap(next_help_heading = "HTTP Server")]
pub struct HttpArgs {
    /// Listen address for the HTTP server.
    #[arg(long, short = 'H', default_value = "localhost:9999")]
    http_addr: String,
}

#[derive(Clone)]
pub struct S {
    pub token: CancellationToken,
    pub pool: Pool<AsyncPgConnection>,
    pub events_base: Url,
}

#[instrument(name = "http", level = "ERROR", skip_all)]
pub async fn run(args: HttpArgs, state: S) -> miette::Result<()> {
    let span = Span::current();
    span.pb_set_style(&crate::PB_STYLE_BLANK);

    debug!(http_addr = args.http_addr, "Binding HTTP listener...");
    let listener = TcpListener::bind(&args.http_addr)
        .await
        .into_diagnostic()
        .context("Couldn't bind socket")?;
    let addr = listener
        .local_addr()
        .into_diagnostic()
        .context("Couldn't get local address")?
        .to_string();
    span.pb_set_message(&addr);
    info!("Running on: http://{addr}/");

    let token = state.token.clone();
    let app = Router::new()
        .route("/", get(root))
        .route("/repos", get(repos).post(repo_create))
        .route("/repos/{kind}", get(repos_by_kind))
        .route("/repos/{kind}/{ident}", get(repo))
        .route("/repos/{kind}/{ident}/storage", put(repo_set_storage))
        .route("/repos/{kind}/{ident}/merges", get(repo_merges))
        .route("/batch/repos/merges", put(batch_repo_merges_import))
        .route(
            "/batch/repos/merges/versions",
            put(batch_repo_merge_versions_import),
        )
        .route("/projects", post(project_create))
        .route("/projects/{project}", get(project))
        .route(
            "/projects/{project}/builds",
            get(project_builds).post(project_build_create),
        )
        .route(
            "/projects/{project}/build-templates/{template}",
            put(build_template_put),
        )
        .route("/projects/{project}/build-templates", get(build_templates))
        .route("/builds", get(builds))
        .route("/builds/{build_id}", get(build))
        .route("/builds/{build_id}/events", get(build_events))
        .route(
            "/builds/{build_id}/internal/heartbeat",
            put(build_internal_heartbeat),
        )
        .route("/builds/{build_id}/internal/end", put(build_internal_end))
        .route(
            "/workers/{worker}/internal/allocate-build",
            put(worker_internal_allocate_build),
        )
        .fallback(error_not_found)
        .method_not_allowed_fallback(error_method_not_allowed)
        .layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(
                move |req: &axum::http::Request<axum::body::Body>| {
                    let (method, path) = (req.method().as_str(), req.uri().path());
                    info_span!(parent: &span, "req", method, path)
                },
            ),
        )
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(token.cancelled_owned())
        .await
        .into_diagnostic()
}

#[derive(Debug, thiserror::Error, Diagnostic)]
#[error(transparent)]
#[diagnostic(transparent)]
pub struct ApiErrorAdapter(ApiError);
impl IntoResponse for ApiErrorAdapter {
    fn into_response(self) -> axum::response::Response {
        (
            self.0.status(),
            ErasedJson::pretty(api::ApiErrorWrapper::from(self.0)),
        )
            .into_response()
    }
}
impl<E: Into<ApiError>> From<E> for ApiErrorAdapter {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

#[derive(Debug)]
pub struct JsonRequest<T: for<'y> Yokeable<'y, Output: Debug + Deserialize<'y>>>(pub Yoke<T>);

impl<T: for<'y> Yokeable<'y, Output: Debug + Deserialize<'y>>, S: Send + Sync> FromRequest<S>
    for JsonRequest<T>
{
    type Rejection = ApiErrorAdapter;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(Yoke::try_attach_to_cart(
            String::from_request(req, state)
                .await
                .map_err(|err| ApiError::BadRequestBody {
                    error: ApiError::wrap_err(&err),
                })?,
            |src| {
                serde_json::from_str(src).map_err(|err| ApiError::BadRequestJson {
                    error: ApiError::wrap_err(&err),
                })
            },
        )?))
    }
}

#[derive(Debug)]
pub struct JsonResponse<T: Debug + Serialize>(pub T);
impl<T: Debug + Serialize> IntoResponse for JsonResponse<T> {
    fn into_response(self) -> axum::response::Response {
        ErasedJson::pretty(self.0).into_response()
    }
}

macro_rules! transaction {
    ($state:ident, $block:expr) => {
        $state
            .pool
            .get()
            .await?
            .transaction(move |conn| Box::pin(async move { ($block)(conn).await }))
    };
}

async fn error_not_found(req: axum::extract::Request) -> Result<()> {
    Err(ApiErrorAdapter::from(ApiError::NoSuchRoute {
        method: req.method().to_string(),
        route: req.uri().path().to_string(),
    }))
}

async fn error_method_not_allowed(req: axum::extract::Request) -> Result<()> {
    Err(ApiErrorAdapter::from(ApiError::MethodNotAllowed {
        method: req.method().to_string(),
        route: req.uri().path().to_string(),
    }))
}

async fn root() -> JsonResponse<api::RootResponse> {
    JsonResponse(api::RootResponse {})
}

async fn repos(State(state): State<S>) -> Result<JsonResponse<api::RepositoriesResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        Ok(JsonResponse(api::RepositoriesResponse {
            repositories: models::Repository::all()
                .load_stream(conn)
                .await?
                .map_ok(|v| v.to_client())
                .try_collect()
                .await?,
        }))
    })
    .await
}

async fn repos_by_kind(
    State(state): State<S>,
    Path(repo_kind): Path<String>,
) -> Result<JsonResponse<api::RepositoriesResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        Ok(JsonResponse(api::RepositoriesResponse {
            repositories: models::Repository::all()
                .filter(schema::gorgond::repositories::kind.eq(repo_kind))
                .load_stream(conn)
                .await?
                .map_ok(|v| v.to_client())
                .try_collect()
                .await?,
        }))
    })
    .await
}

async fn repo(
    State(state): State<S>,
    Path((repo_kind, repo_ident)): Path<(String, String)>,
) -> Result<JsonResponse<api::RepositoryResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        Ok(JsonResponse(api::RepositoryResponse {
            repository: models::Repository::get_ident(conn, &repo_kind, &repo_ident)
                .await?
                .to_client(),
        }))
    })
    .await
}

async fn repo_set_storage(
    State(state): State<S>,
    Path((repo_kind, repo_ident)): Path<(String, String)>,
    JsonRequest(req): JsonRequest<api::RepositorySetStorageRequest<'static>>,
) -> Result<JsonResponse<api::RepositoryResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let repo = diesel::update(schema::gorgond::repositories::table)
            .filter(models::Repository::filter_ident(&repo_kind, &repo_ident))
            .set(schema::gorgond::repositories::storage.eq(&req.get().storage))
            .returning(models::Repository::as_select())
            .get_result::<models::Repository>(conn)
            .await?;
        Ok(JsonResponse(api::RepositoryResponse {
            repository: repo.to_client(),
        }))
    })
    .await
}

async fn repo_create(
    State(state): State<S>,
    JsonRequest(req): JsonRequest<api::RepositoryCreateRequest<'static>>,
) -> Result<JsonResponse<api::RepositoryResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let now = Utc::now();
        let repo =
            models::Repository::from_spec(&req.get().repository.to_owned(), Uuid::now_v7(), now);
        let repo = {
            diesel::insert_into(schema::gorgond::repositories::table)
                .values(&repo)
                .get_result::<models::Repository>(conn)
                .await?
        };
        Ok(JsonResponse(api::RepositoryResponse {
            repository: repo.to_client(),
        }))
    })
    .await
}

async fn repo_merges(
    State(state): State<S>,
    Path((repo_kind, repo_ident)): Path<(String, String)>,
) -> Result<JsonResponse<api::RepositoryMergeRequestsResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let merge_requests = schema::gorgond::repository_merge_requests::table
            .inner_join(schema::gorgond::repositories::table)
            .select(models::RepositoryMergeRequest::as_select())
            .filter(models::Repository::filter_ident(&repo_kind, &repo_ident))
            .load(conn)
            .await?;
        let sources = models::CommitSource::ids(merge_requests.iter().map(|mr| mr.base_id))
            .load_stream(conn)
            .await?
            .map_ok(|src| (src.id, src))
            .try_collect::<BTreeMap<_, _>>()
            .await?;
        Ok(JsonResponse(api::RepositoryMergeRequestsResponse {
            repositories: schema::gorgond::repositories::table
                .select(models::Repository::as_select())
                .filter(
                    schema::gorgond::repositories::id
                        .eq_any(merge_requests.iter().map(|v| v.repository_id)),
                )
                .load_stream(conn)
                .await?
                .map_ok(|v| (v.id, v.to_client()))
                .try_collect()
                .await?,
            merge_requests: (merge_requests.into_iter())
                .filter_map(|v| sources.get(&v.base_id).map(|src| v.to_client(src)))
                .collect(),
        }))
    })
    .await
}

async fn batch_repo_merges_import(
    State(state): State<S>,
    JsonRequest(req): JsonRequest<api::BatchRepositoryMergeRequestsImportRequest<'static>>,
) -> Result<JsonResponse<api::RepositoryMergeRequestsResponse<'static>>> {
    let req = req.get();
    transaction!(state, async |conn: &mut _| {
        let now = Utc::now();

        let repo_ids: BTreeSet<Uuid> = req.merge_requests.keys().copied().collect();
        let repos_f = models::Repository::all()
            .filter(schema::gorgond::repositories::id.eq_any(repo_ids))
            .load_stream(conn);

        // Deduplicate base strings, then upsert them. Postgres doesn't like it if an
        // ON CONFLICT DO UPDATE affects the same row twice in the same query.
        let bases: BTreeSet<(Uuid, &str)> = (req.merge_requests.iter())
            .flat_map(|(repo_id, specs)| specs.iter().map(|spec| (*repo_id, &*spec.base)))
            .collect();
        let bases = (bases.into_iter())
            .map(|(repo_id, base)| {
                models::CommitSource::new(Uuid::now_v7(), repo_id, base.to_string(), now)
            })
            .collect::<Vec<_>>();
        let bases: BTreeMap<String, models::CommitSource> =
            diesel::insert_into(schema::gorgond::commit_sources::table)
                .values(bases)
                .on_conflict((
                    schema::gorgond::commit_sources::repository_id,
                    schema::gorgond::commit_sources::source,
                ))
                .do_update()
                // TODO: Pointless write to force RETURNING.
                .set(excluded!(schema::gorgond::commit_sources::updated_at))
                .returning(models::CommitSource::as_returning())
                .load_stream(conn)
                .await?
                .map_ok(|base| (base.source.clone(), base))
                .try_collect()
                .await?;

        // Insert merge requests.
        let merges = (req.merge_requests.iter())
            .flat_map(|(repo_id, specs)| specs.iter().map(|spec| (*repo_id, spec)))
            .map(|(repo_id, spec)| {
                (bases.get(&*spec.base))
                    .ok_or_else(|| ApiError::ModelNotFound {
                        model: api::Model::CommitSource,
                        key: Some(spec.base.to_string()),
                    })
                    .map(|base| {
                        models::RepositoryMergeRequest::from_spec(
                            spec,
                            Uuid::now_v7(),
                            base.id,
                            repo_id,
                            now,
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let merges = diesel::insert_into(schema::gorgond::repository_merge_requests::table)
            .values(merges)
            .on_conflict((
                schema::gorgond::repository_merge_requests::repository_id,
                schema::gorgond::repository_merge_requests::ident,
            ))
            .do_update()
            .set(excluded!(
                schema::gorgond::repository_merge_requests::ident,
                schema::gorgond::repository_merge_requests::base_id,
                schema::gorgond::repository_merge_requests::title,
                schema::gorgond::repository_merge_requests::description,
                schema::gorgond::repository_merge_requests::closed_at,
                schema::gorgond::repository_merge_requests::updated_at,
            ))
            .returning(models::RepositoryMergeRequest::as_returning())
            .load(conn)
            .await?;

        // Re-map bases by ID.
        let bases: BTreeMap<Uuid, models::CommitSource> =
            bases.into_values().map(|base| (base.id, base)).collect();

        Ok(JsonResponse(api::RepositoryMergeRequestsResponse {
            merge_requests: (merges.into_iter())
                .map(|mr| {
                    (bases.get(&mr.base_id))
                        .ok_or_else(|| ApiError::ModelNotFound {
                            model: api::Model::CommitSource,
                            key: Some(mr.base_id.to_string()),
                        })
                        .map(|base| mr.to_client(base))
                })
                .collect::<Result<_, _>>()?,
            repositories: repos_f
                .await?
                .map_ok(|v| (v.id, v.to_client()))
                .try_collect::<OwnableMap<Uuid, _>>()
                .await?,
        }))
    })
    .await
}

async fn batch_repo_merge_versions_import(
    State(state): State<S>,
    JsonRequest(req): JsonRequest<api::BatchRepositoryMergeRequestVersionsImportRequest<'static>>,
) -> Result<JsonResponse<api::RepositoryMergeRequestVersionsResponse<'static>>> {
    let req = req.get();
    transaction!(state, async |conn: &mut _| {
        let now = Utc::now();

        let merge_ids: BTreeSet<Uuid> = req.merge_request_versions.keys().copied().collect();
        let merges: BTreeMap<Uuid, models::RepositoryMergeRequest> =
            models::RepositoryMergeRequest::all()
                .filter(schema::gorgond::repository_merge_requests::id.eq_any(merge_ids))
                .load_stream(conn)
                .await?
                .map_ok(|mr| (mr.id, mr))
                .try_collect()
                .await?;

        let repo_ids: BTreeSet<Uuid> = merges.values().map(|mr| mr.repository_id).collect();
        let repositories: OwnableMap<Uuid, client::Repository> = models::Repository::all()
            .filter(schema::gorgond::repositories::id.eq_any(repo_ids))
            .load_stream(conn)
            .await?
            .map_ok(|v| (v.id, v.to_client()))
            .try_collect()
            .await?;

        // Cache for source strings. Start it off with merge bases.
        let sources: BTreeMap<String, models::CommitSource> =
            models::CommitSource::ids(merges.values().map(|mr| mr.base_id))
                .load_stream(conn)
                .await?
                .map_ok(|src| (src.source.clone(), src))
                .try_collect()
                .await?;

        // Then insert sources that aren't in that set.
        let mut new_sources = BTreeSet::<(Uuid, &str)>::new();
        for (merge_id, specs) in req.merge_request_versions.iter() {
            let merge = (merges.get(merge_id)).ok_or_else(|| ApiError::ModelNotFound {
                model: api::Model::RepositoryMergeRequest,
                key: Some(merge_id.to_string()),
            })?;
            new_sources.extend(
                (specs.iter())
                    .filter(|spec| !sources.contains_key(&*spec.source))
                    .map(|spec| (merge.repository_id, &*spec.source)),
            );
        }
        let new_sources = (new_sources.into_iter())
            .map(|(repo_id, src)| {
                models::CommitSource::new(Uuid::now_v7(), repo_id, src.to_string(), now)
            })
            .collect::<Vec<_>>();
        let sources = diesel::insert_into(schema::gorgond::commit_sources::table)
            .values(new_sources)
            .on_conflict((
                schema::gorgond::commit_sources::repository_id,
                schema::gorgond::commit_sources::source,
            ))
            .do_update()
            .set(excluded!(schema::gorgond::commit_sources::updated_at))
            .returning(models::CommitSource::as_returning())
            .load_stream(conn)
            .await?
            .try_fold(sources, async |mut sources, src| {
                sources.insert(src.source.clone(), src);
                Ok(sources)
            })
            .await?;

        // Insert merge request versions.
        let versions = (req.merge_request_versions.iter())
            .flat_map(|(merge_id, specs)| {
                specs.iter().map(|spec| {
                    (sources.get(&*spec.source))
                        .ok_or_else(|| ApiError::ModelNotFound {
                            model: api::Model::CommitSource,
                            key: Some(spec.source.to_string()),
                        })
                        .map(|src| {
                            models::RepositoryMergeRequestVersion::from_spec(
                                spec,
                                Uuid::now_v7(),
                                *merge_id,
                                src.id,
                                now,
                            )
                        })
                })
            })
            .collect::<Result<Vec<models::RepositoryMergeRequestVersion>, _>>()?;
        let versions =
            diesel::insert_into(schema::gorgond::repository_merge_request_versions::table)
                .values(versions)
                .on_conflict((
                    schema::gorgond::repository_merge_request_versions::merge_request_id,
                    schema::gorgond::repository_merge_request_versions::ident,
                ))
                .do_update()
                .set(excluded!(
                    schema::gorgond::repository_merge_request_versions::source_id,
                    schema::gorgond::repository_merge_request_versions::current_at,
                    schema::gorgond::repository_merge_request_versions::updated_at,
                ))
                .returning(models::RepositoryMergeRequestVersion::as_returning())
                .load(conn)
                .await?;

        // Re-map sources by ID.
        let sources: BTreeMap<Uuid, models::CommitSource> =
            sources.into_values().map(|src| (src.id, src)).collect();

        Ok(JsonResponse(api::RepositoryMergeRequestVersionsResponse {
            merge_request_versions: (versions.into_iter())
                .map(|v| {
                    (sources.get(&v.source_id))
                        .ok_or_else(|| ApiError::ModelNotFound {
                            model: api::Model::CommitSource,
                            key: Some(v.source_id.to_string()),
                        })
                        .map(|src| v.to_client(src))
                })
                .collect::<Result<_, _>>()?,
            merge_requests: (merges.into_values())
                .map(|mr| {
                    (sources.get(&mr.base_id))
                        .ok_or_else(|| ApiError::ModelNotFound {
                            model: api::Model::CommitSource,
                            key: Some(mr.base_id.to_string()),
                        })
                        .map(|src| (mr.id, mr.to_client(src)))
                })
                .collect::<Result<_, _>>()?,
            repositories,
        }))
    })
    .await
}

async fn project_create(
    State(state): State<S>,
    JsonRequest(req): JsonRequest<api::ProjectCreateRequest<'static>>,
) -> Result<JsonResponse<api::ProjectCreateResponse<'static>>> {
    let spec = &req.get().project.to_owned();
    transaction!(state, async |conn: &mut _| {
        let now = Utc::now();
        let project = diesel::insert_into(schema::gorgond::projects::table)
            .values(&models::Project::from_spec(spec, Uuid::now_v7(), now))
            .returning(models::Project::as_returning())
            .get_result::<models::Project>(conn)
            .await?;

        Ok(JsonResponse(api::ProjectCreateResponse {
            project: project.to_client(),
        }))
    })
    .await
}

async fn project(
    State(state): State<S>,
    Path(project): Path<String>,
) -> Result<JsonResponse<api::ProjectGetResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let project = models::Project::get_ident(conn, &project).await?;
        Ok(JsonResponse(api::ProjectGetResponse {
            project: project.to_client(),
        }))
    })
    .await
}

async fn build_templates(
    State(state): State<S>,
    Path(project): Path<String>,
) -> Result<JsonResponse<api::ProjectBuildTemplatesResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let project = models::Project::get_ident(conn, &project).await?;
        let templates = models::BuildTemplate::belonging_to(&project)
            .load::<models::BuildTemplate>(conn)
            .await?;
        let inputs = models::BuildTemplateInput::belonging_to(&templates)
            .load::<models::BuildTemplateInput>(conn)
            .await?;
        let repos = models::Repository::all()
            .filter(
                schema::gorgond::repositories::id.eq_any(inputs.iter().map(|i| i.repository_id)),
            )
            .load_stream::<models::Repository>(conn)
            .await?
            .map_ok(|r| (r.id, r))
            .try_collect::<BTreeMap<Uuid, models::Repository>>()
            .await?;
        let inputs = inputs.grouped_by(&templates);
        Ok(JsonResponse(api::ProjectBuildTemplatesResponse {
            templates: (templates.into_iter().zip(inputs))
                .map(|(t, is)| {
                    t.to_client(
                        is.into_iter()
                            .filter_map(|i| repos.get(&i.repository_id).map(|r| i.to_client(r))),
                    )
                })
                .collect(),
            project: project.to_client(),
        }))
    })
    .await
}
async fn build_template_put(
    State(state): State<S>,
    Path((project, template)): Path<(String, String)>,
    JsonRequest(req): JsonRequest<api::BuildTemplateSetRequest<'static>>,
) -> Result<JsonResponse<api::BuildTemplateResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let now = Utc::now();
        let spec = req.get().template.to_owned();
        let project = models::Project::get_ident(conn, &project).await?;
        let template = diesel::insert_into(schema::gorgond::build_templates::table)
            .values(&models::BuildTemplate::from_spec(
                &spec,
                template,
                Uuid::now_v7(),
                project.id,
                now,
            ))
            .on_conflict((
                schema::gorgond::build_templates::project_id,
                schema::gorgond::build_templates::slug,
            ))
            .do_update()
            .set(excluded!(
                schema::gorgond::build_templates::kind,
                schema::gorgond::build_templates::expr,
            ))
            .returning(models::BuildTemplate::as_returning())
            .get_result::<models::BuildTemplate>(conn)
            .await?;

        let repos = (spec.inputs.iter())
            .filter_map(|(_, i)| i.try_as_repository_ref().map(|r| r.to_owned()))
            .map(|r| models::Repository::from_spec(&r.to_owned(), Uuid::now_v7(), now))
            .collect::<Vec<_>>();
        let repos = diesel::insert_into(schema::gorgond::repositories::table)
            .values(repos)
            .on_conflict((
                schema::gorgond::repositories::kind,
                schema::gorgond::repositories::ident,
            ))
            // TODO: Pointless write to force RETURNING.
            .do_update()
            .set(
                schema::gorgond::repositories::updated_at.eq(diesel::upsert::excluded(
                    schema::gorgond::repositories::updated_at,
                )),
            )
            .returning(models::Repository::as_returning())
            .load_stream::<models::Repository>(conn)
            .await?
            .map_ok(|r| (r.id, r))
            .try_collect::<BTreeMap<Uuid, models::Repository>>()
            .await?;

        let inputs = diesel::insert_into(schema::gorgond::build_template_inputs::table)
            .values(
                (spec.inputs.into_iter())
                    .filter_map(|(k, i)| {
                        i.try_as_repository_ref().and_then(|rs| {
                            (repos.values())
                                .find(|r| r.kind == rs.kind && r.ident == rs.ident)
                                .map(|r| (k, i.clone(), r.id))
                        })
                    })
                    .map(|(k, i, rid)| {
                        models::BuildTemplateInput::from_spec(
                            &client::BuildTemplateInputSpec {
                                key: k,
                                source: i.clone().into_owned(),
                            },
                            Uuid::now_v7(),
                            template.id,
                            rid,
                            now,
                        )
                    })
                    .collect::<Vec<models::BuildTemplateInput>>(),
            )
            .on_conflict((
                schema::gorgond::build_template_inputs::template_id,
                schema::gorgond::build_template_inputs::key,
            ))
            .do_update()
            .set(excluded!(
                schema::gorgond::build_template_inputs::repository_id,
                schema::gorgond::build_template_inputs::updated_at,
            ))
            .returning(models::BuildTemplateInput::as_returning())
            .load::<models::BuildTemplateInput>(conn)
            .await?;
        diesel::delete(schema::gorgond::build_template_inputs::table)
            .filter(schema::gorgond::build_template_inputs::id.ne_all(inputs.iter().map(|i| i.id)))
            .execute(conn)
            .await?;
        Ok(JsonResponse(api::BuildTemplateResponse {
            template: template.to_client(
                (inputs.into_iter())
                    .filter_map(|i| (repos.get(&i.repository_id).map(|r| i.to_client(r)))),
            ),
            project: project.to_client(),
        }))
    })
    .await
}

async fn project_builds(
    State(state): State<S>,
    Path(project): Path<String>,
) -> Result<JsonResponse<api::ProjectBuildsResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let project = models::Project::get_ident(conn, &project).await?;
        let builds = models::Build::belonging_to(&project)
            .load::<models::Build>(conn)
            .await?;
        let build_inputs = models::BuildInput::belonging_to(&builds)
            .load::<models::BuildInput>(conn)
            .await?
            .grouped_by(&builds);
        Ok(JsonResponse(api::ProjectBuildsResponse {
            project: project.to_client(),
            builds: builds
                .into_iter()
                .zip(build_inputs)
                .map(|(b, bi)| b.to_client(bi))
                .collect(),
        }))
    })
    .await
}

async fn project_build_create(
    State(state): State<S>,
    Path(project): Path<String>,
    JsonRequest(req): JsonRequest<api::ProjectBuildCreateRequest<'static>>,
) -> Result<JsonResponse<api::ProjectBuildCreateResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let now = Utc::now();
        let project_id = models::Project::get_id(conn, &project).await?;

        let (build, inputs) =
            models::Build::from_spec(req.get().build.to_owned(), Uuid::now_v7(), project_id, now);
        let build = {
            use schema::gorgond::builds;
            diesel::insert_into(builds::table)
                .values(&build)
                .get_result::<models::Build>(conn)
                .await?
        };
        let inputs = {
            use schema::gorgond::build_inputs;
            diesel::insert_into(build_inputs::table)
                .values(inputs.collect::<Vec<_>>())
                .get_results::<models::BuildInput>(conn)
                .await?
        };
        Ok(JsonResponse(api::ProjectBuildCreateResponse {
            build: build.to_client(inputs),
        }))
    })
    .await
}

async fn builds(State(state): State<S>) -> Result<JsonResponse<api::BuildsResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let builds = models::Build::all().get_results(conn).await?;
        let build_inputs = models::BuildInput::belonging_to(&builds)
            .load::<models::BuildInput>(conn)
            .await?
            .grouped_by(&builds);

        Ok(JsonResponse(api::BuildsResponse {
            workers: models::Worker::ids(builds.iter().filter_map(|b| b.worker_id))
                .load_stream(conn)
                .await?
                .map_ok(|w| (w.id, w.to_client()))
                .try_collect()
                .await?,
            projects: models::Project::ids(builds.iter().map(|b| b.project_id))
                .load_stream(conn)
                .await?
                .map_ok(|p| (p.id, p.to_client()))
                .try_collect()
                .await?,
            builds: builds
                .into_iter()
                .zip(build_inputs)
                .map(|(b, bi)| b.to_client(bi))
                .collect(),
        }))
    })
    .await
}

async fn build(
    State(state): State<S>,
    Path(build_id): Path<Uuid>,
) -> Result<JsonResponse<api::BuildResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let build = models::Build::get(conn, build_id).await?;
        let build_inputs = models::BuildInput::belonging_to(&build)
            .load::<models::BuildInput>(conn)
            .await?;

        Ok(JsonResponse(api::BuildResponse {
            worker: match build.worker_id {
                Some(worker_id) => Some(models::Worker::get(conn, worker_id).await?.to_client()),
                None => None,
            },
            project: models::Project::get(conn, build.project_id)
                .await?
                .to_client(),
            build: build.to_client(build_inputs),
        }))
    })
    .await
}

async fn build_events(
    State(state): State<S>,
    Path(build_id): Path<Uuid>,
) -> Result<JsonResponse<api::BuildEventsResponse>> {
    Ok(JsonResponse(api::BuildEventsResponse {
        url: state.events_base.join(&format!("{build_id}.cbor.zst"))?,
    }))
}

async fn build_internal_heartbeat(
    State(state): State<S>,
    Path(build_id): Path<Uuid>,
    JsonRequest(req): JsonRequest<api::BuildInternalHeartbeatRequest>,
) -> Result<JsonResponse<api::BuildInternalHeartbeatResponse>> {
    let req = req.get();
    transaction!(state, async |conn: &mut _| {
        diesel::update(schema::gorgond::builds::table)
            .filter(schema::gorgond::builds::id.eq(build_id))
            .set((
                schema::gorgond::builds::started_at.eq(req.started_at),
                schema::gorgond::builds::heartbeat_at.eq(req.heartbeat_at),
            ))
            .execute(conn)
            .await?;

        Ok(JsonResponse(api::BuildInternalHeartbeatResponse {}))
    })
    .await
}
async fn build_internal_end(
    State(state): State<S>,
    Path(build_id): Path<Uuid>,
    JsonRequest(req): JsonRequest<api::BuildInternalEndRequest>,
) -> Result<JsonResponse<api::BuildInternalEndResponse>> {
    let req = req.get();
    transaction!(state, async |conn: &mut _| {
        diesel::update(schema::gorgond::builds::table)
            .filter(schema::gorgond::builds::id.eq(build_id))
            .set((
                schema::gorgond::builds::started_at.eq(req.started_at),
                schema::gorgond::builds::ended_at.eq(req.ended_at),
                schema::gorgond::builds::result.eq(models::BuildResult::from(req.result)),
            ))
            .execute(conn)
            .await?;

        Ok(JsonResponse(api::BuildInternalEndResponse {}))
    })
    .await
}

async fn worker_internal_allocate_build(
    State(state): State<S>,
    Path(worker): Path<String>,
    JsonRequest(_req): JsonRequest<api::WorkerInternalAllocateBuildRequest>,
) -> Result<JsonResponse<api::WorkerInternalAllocateBuildResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        use schema::gorgond::{build_inputs, builds, workers};
        let now = Utc::now();

        // Resolve the worker slug to an ID, or create it if it doesn't exist.
        let worker_id = {
            let default_worker = models::Worker {
                id: Uuid::now_v7(),
                slug: worker.clone(),
                name: worker.clone(),
                created_at: now,
                updated_at: now,
                seen_at: Some(now),
            };
            // Without *a lot* of persuading, Postgres returns nothing to a query with
            // ON CONFLICT DO NOTHING and no changes, so we bump seen_at here instead.
            let worker_id = diesel::insert_into(workers::table)
                .values(&default_worker)
                .on_conflict(workers::slug)
                .do_update()
                .set(workers::seen_at.eq(now))
                .returning(workers::id)
                .get_result::<Uuid>(conn)
                .await?;
            if worker_id == default_worker.id {
                info!("New worker node registered!");
            }
            worker_id
        };

        // Try to find a build for the worker to do.
        let build = if let Some(build) = models::Build::all()
            .filter(builds::result.is_null())
            .filter(builds::worker_id.eq(worker_id))
            .first::<models::Build>(conn)
            .await
            .optional()?
        {
            info!("Worker has an unfinished build, reminding it");
            build
        } else if let Some(build_id) = builds::table
            .filter(builds::worker_id.is_null())
            .select(builds::id)
            .limit(1)
            .order_by(builds::created_at.asc())
            .for_no_key_update() // No race conditions pls.
            .get_result::<Uuid>(conn)
            .await
            .optional()?
        {
            info!("Found an unallocated build, allocating it");

            // ...and allocate it to this worker.
            debug!("Allocating build...");
            diesel::update(builds::table)
                .filter(builds::id.eq(build_id))
                .set(builds::worker_id.eq(worker_id))
                .returning(models::Build::as_returning())
                .get_result(conn)
                .await?
        } else {
            warn!("No work available!");
            return Err(ApiError::NoWorkAvailable.into());
        };

        let inputs = build_inputs::table
            .filter(build_inputs::build_id.eq(build.id))
            .get_results::<models::BuildInput>(conn)
            .await?;
        Ok(JsonResponse(api::WorkerInternalAllocateBuildResponse {
            build: build.to_client(inputs),
        }))
    })
    .await
}
