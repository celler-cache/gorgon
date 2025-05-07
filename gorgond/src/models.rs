// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{borrow::Cow, collections::BTreeMap};

use chrono::prelude::*;
use diesel::{
    dsl::{self, auto_type},
    pg::Pg,
    prelude::*,
    Selectable,
};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use diesel_derive_enum::DbEnum;
use either::Either;
use gorgond_client::{api, events, models as client};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yoke::Yokeable;

use crate::schema;

macro_rules! impl_model {
    ($model:ident, $schema:ident) => {
        impl $model {
            pub fn all() -> dsl::Select<schema::$schema::table, dsl::AsSelect<Self, Pg>> {
                schema::$schema::table.select(Self::as_select())
            }

            #[auto_type(no_type_alias)]
            pub fn ids<I: IntoIterator<Item = Uuid>>(ids: I) -> _ {
                let q: dsl::Select<schema::$schema::table, dsl::AsSelect<Self, Pg>> = Self::all();
                let it: I::IntoIter = ids.into_iter();
                q.filter(schema::$schema::id.eq_any(it))
            }

            pub async fn get(
                conn: &mut AsyncPgConnection,
                id: Uuid,
            ) -> Result<Self, api::ApiError> {
                Self::all()
                    .find(id)
                    .first(conn)
                    .await
                    .optional()?
                    .ok_or_else(|| api::ApiError::ModelNotFound {
                        model: api::Model::$model,
                        key: Some(id.to_string()),
                    })
            }
        }
    };
}
macro_rules! impl_model_slug {
    ($model:ident, $schema:ident) => {
        impl $model {
            #[auto_type(no_type_alias)]
            pub fn with_ident(ident: &str) -> _ {
                let id: Uuid = Uuid::parse_str(&ident).unwrap_or_default();
                let q: dsl::Select<schema::$schema::table, dsl::AsSelect<Self, Pg>> = Self::all();
                q.find(id).or_filter(schema::$schema::slug.eq(ident))
            }

            pub async fn get_ident(
                conn: &mut AsyncPgConnection,
                ident: &str,
            ) -> Result<Self, api::ApiError> {
                Self::with_ident(ident)
                    .first(conn)
                    .await
                    .optional()?
                    .ok_or_else(|| api::ApiError::ModelNotFound {
                        model: api::Model::$model,
                        key: Some(ident.into()),
                    })
            }
            pub async fn get_id(
                conn: &mut AsyncPgConnection,
                ident: &str,
            ) -> Result<Uuid, api::ApiError> {
                Self::with_ident(ident)
                    .select(schema::$schema::id)
                    .first(conn)
                    .await
                    .optional()?
                    .ok_or_else(|| api::ApiError::ModelNotFound {
                        model: api::Model::$model,
                        key: Some(ident.into()),
                    })
            }
        }
    };
}

#[derive(
    Debug, Serialize, Deserialize, Yokeable, Queryable, Selectable, Insertable, Identifiable,
)]
#[diesel(table_name = crate::schema::projects, check_for_backend(diesel::pg::Pg))]
pub struct Project {
    pub id: Uuid,
    pub slug: String,
    pub name: String,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl_model!(Project, projects);
impl_model_slug!(Project, projects);

impl Project {
    pub fn from_spec(v: client::ProjectSpec<'static>, id: Uuid, now: DateTime<Utc>) -> Self {
        Self {
            id,
            slug: v.slug.into(),
            name: v.name.into(),
            created_at: now,
            updated_at: now,
        }
    }
    pub fn to_client(self) -> client::Project<'static> {
        client::Project {
            id: self.id,
            spec: client::ProjectSpec {
                slug: self.slug.into(),
                name: self.name.into(),
            },
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(
    Debug,
    Serialize,
    Deserialize,
    Yokeable,
    Queryable,
    QueryableByName,
    Selectable,
    Insertable,
    Identifiable,
    AsChangeset,
)]
#[diesel(table_name = crate::schema::workers, check_for_backend(diesel::pg::Pg))]
pub struct Worker {
    pub id: Uuid,
    pub slug: String,
    pub name: String,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl_model!(Worker, workers);
impl_model_slug!(Worker, workers);

impl Worker {
    pub fn to_client(self) -> client::Worker<'static> {
        client::Worker {
            id: self.id,
            spec: client::WorkerSpec {
                slug: self.slug.into(),
                name: self.name.into(),
            },
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, DbEnum)]
#[db_enum(existing_type_path = "crate::schema::sql_types::BuildResult")]
pub enum BuildResult {
    Success,
    Failure,
    WorkerLost,
}
impl From<client::BuildResult> for BuildResult {
    fn from(v: client::BuildResult) -> Self {
        match v {
            client::BuildResult::Success => Self::Success,
            client::BuildResult::Failure => Self::Failure,
            client::BuildResult::WorkerLost => Self::WorkerLost,
        }
    }
}
impl From<BuildResult> for client::BuildResult {
    fn from(v: BuildResult) -> Self {
        match v {
            BuildResult::Success => Self::Success,
            BuildResult::Failure => Self::Failure,
            BuildResult::WorkerLost => Self::WorkerLost,
        }
    }
}

#[derive(
    Debug,
    Serialize,
    Deserialize,
    Yokeable,
    Queryable,
    Selectable,
    Insertable,
    Identifiable,
    Associations,
)]
#[diesel(table_name = crate::schema::build_inputs, check_for_backend(diesel::pg::Pg))]
#[diesel(belongs_to(Build))]
pub struct BuildInput {
    pub id: Uuid,
    pub build_id: Uuid,
    pub key: String,
    pub source: String,
}
impl_model!(BuildInput, build_inputs);

#[derive(
    Debug,
    Serialize,
    Deserialize,
    Yokeable,
    Queryable,
    Selectable,
    Insertable,
    Identifiable,
    Associations,
)]
#[diesel(table_name = crate::schema::builds, check_for_backend(diesel::pg::Pg))]
#[diesel(belongs_to(Project), belongs_to(Worker))]
pub struct Build {
    pub id: Uuid,
    pub project_id: Uuid,
    pub kind: String,
    pub source: String,
    pub expr: String,

    pub worker_id: Option<Uuid>,
    pub result: Option<BuildResult>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub allocated_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}
impl_model!(Build, builds);

impl Build {
    pub fn from_spec(
        v: client::BuildSpec<'static>,
        id: Uuid,
        project_id: Uuid,
        now: DateTime<Utc>,
    ) -> (Self, impl Iterator<Item = BuildInput>) {
        (
            Self {
                id,
                project_id,
                kind: v.kind.into(),
                source: v.source.into(),
                expr: v.expr.into(),
                worker_id: None,
                result: None,
                created_at: now,
                updated_at: now,
                allocated_at: None,
                heartbeat_at: None,
                started_at: None,
                ended_at: None,
            },
            v.inputs.into_iter().map(move |(k, v)| BuildInput {
                id: Uuid::now_v7(),
                build_id: id,
                key: k.into(),
                source: v.into(),
            }),
        )
    }
    pub fn to_client(self, inputs: impl IntoIterator<Item = BuildInput>) -> client::Build<'static> {
        client::Build {
            id: self.id,
            project_id: self.project_id,
            spec: client::BuildSpec {
                kind: self.kind.into(),
                inputs: BTreeMap::from_iter(
                    inputs
                        .into_iter()
                        .map(|input| (input.key.into(), input.source.into())),
                ),
                source: self.source.into(),
                expr: self.expr.into(),
            },
            worker_id: self.worker_id,
            result: self.result.map(|v| v.into()),
            created_at: self.created_at,
            updated_at: self.updated_at,
            allocated_at: self.allocated_at,
            heartbeat_at: self.heartbeat_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, DbEnum)]
#[db_enum(existing_type_path = "crate::schema::sql_types::BuildTaskLevel")]
pub enum BuildTaskLevel {
    Error,
    Warn,
    Notice,
    Info,
    Talkative,
    Chatty,
    Debug,
    Vomit,
}
impl From<client::BuildTaskLevel> for BuildTaskLevel {
    fn from(level: client::BuildTaskLevel) -> Self {
        match level {
            client::BuildTaskLevel::Error => Self::Error,
            client::BuildTaskLevel::Warn => Self::Warn,
            client::BuildTaskLevel::Notice => Self::Notice,
            client::BuildTaskLevel::Info => Self::Info,
            client::BuildTaskLevel::Talkative => Self::Talkative,
            client::BuildTaskLevel::Chatty => Self::Chatty,
            client::BuildTaskLevel::Debug => Self::Debug,
            client::BuildTaskLevel::Vomit => Self::Vomit,
        }
    }
}
impl From<BuildTaskLevel> for client::BuildTaskLevel {
    fn from(level: BuildTaskLevel) -> Self {
        match level {
            BuildTaskLevel::Error => Self::Error,
            BuildTaskLevel::Warn => Self::Warn,
            BuildTaskLevel::Notice => Self::Notice,
            BuildTaskLevel::Info => Self::Info,
            BuildTaskLevel::Talkative => Self::Talkative,
            BuildTaskLevel::Chatty => Self::Chatty,
            BuildTaskLevel::Debug => Self::Debug,
            BuildTaskLevel::Vomit => Self::Vomit,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, DbEnum)]
#[db_enum(existing_type_path = "crate::schema::sql_types::BuildTaskKind")]
pub enum BuildTaskKind {
    Helper,
    Realise,
    Builds,
    Build,
    PostBuildHook,
    BuildWaiting,
    Substitute,
    CopyPaths,
    CopyPath,
    FileTransfer,
    OptimiseStore,
    VerifyPaths,
    QueryPathInfo,
}
impl From<client::BuildTaskKind> for BuildTaskKind {
    fn from(kind: client::BuildTaskKind) -> Self {
        match kind {
            client::BuildTaskKind::Helper => Self::Helper,
            client::BuildTaskKind::Realise => Self::Realise,
            client::BuildTaskKind::Builds => Self::Builds,
            client::BuildTaskKind::Build => Self::Build,
            client::BuildTaskKind::PostBuildHook => Self::PostBuildHook,
            client::BuildTaskKind::BuildWaiting => Self::BuildWaiting,
            client::BuildTaskKind::Substitute => Self::Substitute,
            client::BuildTaskKind::CopyPaths => Self::CopyPaths,
            client::BuildTaskKind::CopyPath => Self::CopyPath,
            client::BuildTaskKind::FileTransfer => Self::FileTransfer,
            client::BuildTaskKind::OptimiseStore => Self::OptimiseStore,
            client::BuildTaskKind::VerifyPaths => Self::VerifyPaths,
            client::BuildTaskKind::QueryPathInfo => Self::QueryPathInfo,
        }
    }
}
impl From<BuildTaskKind> for client::BuildTaskKind {
    fn from(kind: BuildTaskKind) -> Self {
        match kind {
            BuildTaskKind::Helper => Self::Helper,
            BuildTaskKind::Realise => Self::Realise,
            BuildTaskKind::Builds => Self::Builds,
            BuildTaskKind::Build => Self::Build,
            BuildTaskKind::PostBuildHook => Self::PostBuildHook,
            BuildTaskKind::BuildWaiting => Self::BuildWaiting,
            BuildTaskKind::Substitute => Self::Substitute,
            BuildTaskKind::CopyPaths => Self::CopyPaths,
            BuildTaskKind::CopyPath => Self::CopyPath,
            BuildTaskKind::FileTransfer => Self::FileTransfer,
            BuildTaskKind::OptimiseStore => Self::OptimiseStore,
            BuildTaskKind::VerifyPaths => Self::VerifyPaths,
            BuildTaskKind::QueryPathInfo => Self::QueryPathInfo,
        }
    }
}

#[derive(
    Debug,
    Serialize,
    Deserialize,
    Yokeable,
    Queryable,
    Selectable,
    Insertable,
    Identifiable,
    Associations,
)]
#[diesel(table_name = crate::schema::build_tasks, check_for_backend(diesel::pg::Pg))]
#[diesel(belongs_to(Build), belongs_to(BuildTask, foreign_key=parent_id))]
pub struct BuildTask {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub build_id: Uuid,

    pub name: String,
    pub kind: Option<BuildTaskKind>,
    pub level: BuildTaskLevel,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}
impl_model!(BuildTask, build_tasks);

impl BuildTask {
    pub fn from_client(task: client::BuildTask) -> Self {
        let now = Utc::now();
        Self {
            id: task.id,
            parent_id: task.parent_id,
            build_id: task.build_id,
            name: task.name.into(),
            kind: task.kind.map(|v| v.into()),
            level: task.level.into(),
            created_at: now,
            updated_at: now,
            started_at: Some(task.started_at),
            ended_at: task.ended_at,
        }
    }
    pub fn to_client(self) -> client::BuildTask<'static> {
        client::BuildTask {
            id: self.id,
            parent_id: self.parent_id,
            build_id: self.build_id,
            name: self.name.into(),
            kind: self.kind.map(|v| v.into()),
            level: self.level.into(),
            created_at: Some(self.created_at),
            updated_at: Some(self.updated_at),
            started_at: self.started_at.unwrap_or(self.created_at),
            ended_at: self.ended_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, DbEnum)]
#[db_enum(existing_type_path = "crate::schema::sql_types::BuildEventKind")]
pub enum BuildEventKind {
    Started,
    Stopped,
    FileLinked,
    UntrustedPath,
    CorruptedPath,
    SetPhase,
    Progress,
}

#[derive(
    Debug,
    Serialize,
    Deserialize,
    Yokeable,
    Queryable,
    Selectable,
    Insertable,
    Identifiable,
    Associations,
)]
#[diesel(table_name = crate::schema::build_events, check_for_backend(diesel::pg::Pg))]
#[diesel(belongs_to(Build), belongs_to(BuildTask, foreign_key=task_id))]
pub struct BuildEvent {
    pub id: Uuid,
    pub build_id: Uuid,
    pub task_id: Option<Uuid>,

    pub kind: BuildEventKind,

    pub s0: String,
    pub i0: Option<i64>,
    pub i1: Option<i64>,
    pub i2: Option<i64>,
    pub i3: Option<i64>,

    pub created_at: DateTime<Utc>,
    pub timestamp: DateTime<Utc>,
}
impl_model!(BuildEvent, build_events);

impl BuildEvent {
    pub fn from_client(e: events::BuildEvent) -> Either<(Self, Option<BuildTask>), BuildLog> {
        let mut slf = Self {
            id: e.event_id,
            build_id: e.build_id,
            task_id: None,
            kind: BuildEventKind::Started,
            s0: String::new(),
            i0: None,
            i1: None,
            i2: None,
            i3: None,
            created_at: Utc::now(),
            timestamp: e.timestamp,
        };
        let mut task = None;
        slf.kind = match e.event {
            events::BuildEventKind::Started => BuildEventKind::Started,
            events::BuildEventKind::Stopped => BuildEventKind::Stopped,
            events::BuildEventKind::Task(events::BuildTaskEvent { task_id, event }) => {
                slf.task_id = Some(task_id);
                match event {
                    events::BuildTaskEventKind::Started(t) => {
                        task = Some({
                            let mut task = BuildTask::from_client(t);
                            task.id = task_id;
                            task.build_id = slf.build_id;
                            task
                        });
                        BuildEventKind::Started
                    }
                    events::BuildTaskEventKind::Stopped => BuildEventKind::Stopped,
                    events::BuildTaskEventKind::Updated(up) => match up {
                        events::BuildTaskUpdate::FileLinked { bytes } => {
                            slf.i0 = Some(bytes.into());
                            BuildEventKind::FileLinked
                        }
                        events::BuildTaskUpdate::UntrustedPath => BuildEventKind::UntrustedPath,
                        events::BuildTaskUpdate::CorruptedPath => BuildEventKind::CorruptedPath,
                        events::BuildTaskUpdate::SetPhase { phase } => {
                            slf.s0 = phase.into();
                            BuildEventKind::SetPhase
                        }
                        events::BuildTaskUpdate::Progress {
                            done,
                            expected,
                            running,
                            failed,
                        } => {
                            slf.i0 = Some(done.into());
                            slf.i1 = Some(expected.into());
                            slf.i2 = Some(running.into());
                            slf.i3 = Some(failed.into());
                            BuildEventKind::Progress
                        }
                    },
                    events::BuildTaskEventKind::Log(log) => {
                        return Either::Right(BuildLog::from_partial_event(slf, log))
                    }
                }
            }
            events::BuildEventKind::Log(log) => {
                return Either::Right(BuildLog::from_partial_event(slf, log))
            }
        };
        Either::Left((slf, task))
    }
    pub fn to_client(
        self,
        task: Option<&client::BuildTask<'static>>,
    ) -> events::BuildEvent<'static> {
        let for_task = |event| {
            events::BuildEventKind::Task(events::BuildTaskEvent {
                task_id: self.task_id.unwrap_or_default(),
                event,
            })
        };
        let update = |update| for_task(events::BuildTaskEventKind::Updated(update));
        events::BuildEvent {
            timestamp: self.timestamp,
            build_id: self.build_id,
            event_id: self.id,
            event: match self.kind {
                BuildEventKind::Started => match task {
                    Some(task) => for_task(events::BuildTaskEventKind::Started(task.clone())),
                    None => events::BuildEventKind::Started,
                },
                BuildEventKind::Stopped => match task {
                    Some(_) => for_task(events::BuildTaskEventKind::Stopped),
                    None => events::BuildEventKind::Stopped,
                },
                BuildEventKind::FileLinked => update(events::BuildTaskUpdate::FileLinked {
                    bytes: self.i0.unwrap_or_default() as _,
                }),
                BuildEventKind::UntrustedPath => update(events::BuildTaskUpdate::UntrustedPath),
                BuildEventKind::CorruptedPath => update(events::BuildTaskUpdate::CorruptedPath),
                BuildEventKind::SetPhase => update(events::BuildTaskUpdate::SetPhase {
                    phase: self.s0.into(),
                }),
                BuildEventKind::Progress => update(events::BuildTaskUpdate::Progress {
                    done: self.i0.unwrap_or_default() as _,
                    expected: self.i1.unwrap_or_default() as _,
                    running: self.i2.unwrap_or_default() as _,
                    failed: self.i3.unwrap_or_default() as _,
                }),
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, DbEnum)]
#[db_enum(existing_type_path = "crate::schema::sql_types::BuildLogKind")]
pub enum BuildLogKind {
    Build,
    PostBuild,
    Message,
}
impl From<events::LogKind> for BuildLogKind {
    fn from(kind: events::LogKind) -> Self {
        match kind {
            events::LogKind::Build => Self::Build,
            events::LogKind::PostBuild => Self::PostBuild,
            events::LogKind::Message => Self::Message,
        }
    }
}
impl From<BuildLogKind> for events::LogKind {
    fn from(kind: BuildLogKind) -> Self {
        match kind {
            BuildLogKind::Build => Self::Build,
            BuildLogKind::PostBuild => Self::PostBuild,
            BuildLogKind::Message => Self::Message,
        }
    }
}

#[derive(
    Debug,
    Serialize,
    Deserialize,
    Yokeable,
    Queryable,
    Selectable,
    Insertable,
    Identifiable,
    Associations,
)]
#[diesel(table_name = crate::schema::build_logs, check_for_backend(diesel::pg::Pg))]
#[diesel(belongs_to(Build), belongs_to(BuildTask, foreign_key=task_id))]
pub struct BuildLog {
    pub id: Uuid,
    pub build_id: Uuid,
    pub task_id: Option<Uuid>,

    pub level: BuildTaskLevel,
    pub kind: BuildLogKind,
    pub file: String,
    pub col: Option<i64>,
    pub line: Option<i64>,
    pub txt: String,

    pub created_at: DateTime<Utc>,
    pub timestamp: DateTime<Utc>,
}
impl_model!(BuildLog, build_logs);

impl BuildLog {
    fn from_partial_event(event: BuildEvent, log: events::Log) -> Self {
        Self {
            id: event.id,
            build_id: event.build_id,
            task_id: event.task_id,
            level: log.level.into(),
            kind: log.kind.into(),
            file: log.file.map(|v| v.into()).unwrap_or_default(),
            col: log.column.map(|v| v.into()),
            line: log.line.map(|v| v.into()),
            txt: log.text.into(),
            created_at: event.created_at,
            timestamp: event.timestamp,
        }
    }
    pub fn to_client(self) -> events::BuildEvent<'static> {
        events::BuildEvent {
            timestamp: self.timestamp,
            build_id: self.build_id,
            event_id: self.id,
            event: events::BuildEventKind::Log(events::Log {
                level: self.level.into(),
                kind: self.kind.into(),
                file: Some(Cow::<str>::Owned(self.file)).filter(|s| !s.is_empty()),
                column: self.col.map(|v| v as _),
                line: self.line.map(|v| v as _),
                text: self.txt.into(),
            }),
        }
    }
}
