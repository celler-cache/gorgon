// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use chrono::prelude::*;
use ownable::{IntoOwned, ToOwned};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::BTreeMap, fmt::Debug};
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};
use uuid::Uuid;
use yoke::Yokeable;

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct ProjectSpec<'a> {
    #[serde(borrow)]
    pub slug: Cow<'a, str>,
    #[serde(borrow)]
    pub name: Cow<'a, str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct Project<'a> {
    #[ownable(clone)]
    pub id: Uuid,
    #[serde(flatten, borrow)]
    pub spec: ProjectSpec<'a>,

    #[ownable(clone)]
    pub created_at: DateTime<Utc>,
    #[ownable(clone)]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct WorkerSpec<'a> {
    #[serde(borrow)]
    pub slug: Cow<'a, str>,
    #[serde(borrow)]
    pub name: Cow<'a, str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct Worker<'a> {
    #[ownable(clone)]
    pub id: Uuid,
    #[serde(flatten, borrow)]
    pub spec: WorkerSpec<'a>,

    #[ownable(clone)]
    pub created_at: DateTime<Utc>,
    #[ownable(clone)]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToOwned, IntoOwned)]
#[serde(rename_all = "snake_case")]
pub enum BuildResult {
    Success,
    Failure,
    WorkerLost,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct BuildSpec<'a> {
    #[serde(borrow)]
    pub kind: Cow<'a, str>,
    #[serde(borrow)]
    pub inputs: BTreeMap<Cow<'a, str>, Cow<'a, str>>,
    #[serde(borrow)]
    pub source: Cow<'a, str>,
    #[serde(borrow)]
    pub expr: Cow<'a, str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct Build<'a> {
    #[ownable(clone)]
    pub id: Uuid,
    #[ownable(clone)]
    pub project_id: Uuid,
    #[serde(flatten, borrow)]
    pub spec: BuildSpec<'a>,

    #[ownable(clone)]
    pub worker_id: Option<Uuid>,
    pub result: Option<BuildResult>,

    #[ownable(clone)]
    pub created_at: DateTime<Utc>,
    #[ownable(clone)]
    pub updated_at: DateTime<Utc>,
    #[ownable(clone)]
    pub allocated_at: Option<DateTime<Utc>>,
    #[ownable(clone)]
    pub heartbeat_at: Option<DateTime<Utc>>,
    #[ownable(clone)]
    pub started_at: Option<DateTime<Utc>>,
    #[ownable(clone)]
    pub ended_at: Option<DateTime<Utc>>,
}

/// Verbosity levels for a Task, including all of Nix' levels.
#[derive(
    Debug,
    Display,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    EnumString,
    AsRefStr,
    IntoStaticStr,
    ToOwned,
    IntoOwned,
)]
pub enum TaskLevel {
    #[serde(rename = "E")]
    #[strum(serialize = "E")]
    Error,
    #[serde(rename = "W")]
    #[strum(serialize = "W")]
    Warn,
    #[serde(rename = "N")]
    #[strum(serialize = "N")]
    Notice,
    #[serde(rename = "I")]
    #[strum(serialize = "I")]
    #[default]
    Info,
    #[serde(rename = "T")]
    #[strum(serialize = "T")]
    Talkative,
    #[serde(rename = "C")]
    #[strum(serialize = "C")]
    Chatty,
    #[serde(rename = "D")]
    #[strum(serialize = "D")]
    Debug,
    #[serde(rename = "V")]
    #[strum(serialize = "V")]
    Vomit,
}
#[cfg(feature = "log")]
impl From<TaskLevel> for log::Level {
    fn from(level: TaskLevel) -> Self {
        match level {
            TaskLevel::Error => log::Level::Error,
            TaskLevel::Warn => log::Level::Warn,
            TaskLevel::Notice | TaskLevel::Info => log::Level::Info,
            TaskLevel::Talkative | TaskLevel::Chatty | TaskLevel::Debug => log::Level::Debug,
            TaskLevel::Vomit => log::Level::Trace,
        }
    }
}

/// Every possible kind of task.
#[derive(
    Debug,
    Display,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    EnumString,
    AsRefStr,
    IntoStaticStr,
    ToOwned,
    IntoOwned,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
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

/// A task performed as part of a Build.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct Task<'a> {
    /// ID of the parent Build.
    #[ownable(clone)]
    pub build_id: Uuid,
    /// Sub-ID of this task. Unique together with build_id, but may be reused between builds.
    /// This may be a UUIDv8 with a build system specific ID unceremoniously shoved into it.
    #[ownable(clone)]
    pub task_sid: Uuid,
    /// Sub-ID of the parent task, if any. The parent task must have the same build_id.
    #[ownable(clone)]
    pub parent_sid: Option<Uuid>,

    #[serde(borrow)]
    pub name: Cow<'a, str>,
    pub kind: Option<TaskKind>,
    pub level: Option<TaskLevel>,

    #[ownable(clone)]
    pub start: DateTime<Utc>,
    #[ownable(clone)]
    pub end: Option<DateTime<Utc>>,
}
