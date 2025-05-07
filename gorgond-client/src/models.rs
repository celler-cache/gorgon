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

#[derive(Debug, Display, Clone, Copy, Serialize, Deserialize, ToOwned, IntoOwned)]
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
pub enum BuildTaskLevel {
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
impl From<BuildTaskLevel> for log::Level {
    fn from(level: BuildTaskLevel) -> Self {
        match level {
            BuildTaskLevel::Error => log::Level::Error,
            BuildTaskLevel::Warn => log::Level::Warn,
            BuildTaskLevel::Notice | BuildTaskLevel::Info => log::Level::Info,
            BuildTaskLevel::Talkative | BuildTaskLevel::Chatty | BuildTaskLevel::Debug => {
                log::Level::Debug
            }
            BuildTaskLevel::Vomit => log::Level::Trace,
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

/// A task performed as part of a Build.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct BuildTask<'a> {
    #[ownable(clone)]
    pub id: Uuid,
    #[ownable(clone)]
    pub parent_id: Option<Uuid>,
    #[ownable(clone)]
    pub build_id: Uuid,

    #[serde(borrow)]
    pub name: Cow<'a, str>,
    pub kind: Option<BuildTaskKind>,
    pub level: BuildTaskLevel,

    #[ownable(clone)]
    pub created_at: Option<DateTime<Utc>>,
    #[ownable(clone)]
    pub updated_at: Option<DateTime<Utc>>,
    #[ownable(clone)]
    pub started_at: DateTime<Utc>,
    #[ownable(clone)]
    pub ended_at: Option<DateTime<Utc>>,
}
impl<'a> BuildTask<'a> {
    pub fn borrowed(&'a self) -> Self {
        Self {
            id: self.id,
            parent_id: self.parent_id,
            build_id: self.build_id,
            name: Cow::Borrowed(&self.name),
            kind: self.kind,
            level: self.level,
            created_at: self.created_at,
            updated_at: self.updated_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
        }
    }
}
