// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::borrow::Cow;

use chrono::prelude::*;
use ownable::{IntoOwned, ToOwned};
use serde::{Deserialize, Serialize};
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};
use uuid::Uuid;
use yoke::Yokeable;

use crate::models;

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct BuildEvent<'a> {
    #[ownable(clone)]
    pub timestamp: DateTime<Utc>,
    #[ownable(clone)]
    pub build_id: Uuid,
    #[ownable(clone)]
    pub event_id: Uuid,
    #[serde(borrow)]
    pub event: BuildEventKind<'a>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToOwned, IntoOwned)]
#[serde(rename_all = "snake_case")]
pub enum BuildEventKind<'a> {
    Started,
    Stopped,
    #[serde(borrow)]
    Task(BuildTaskEvent<'a>),
    #[serde(borrow)]
    Log(Log<'a>),
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct BuildTaskEvent<'a> {
    #[ownable(clone)]
    pub task_id: Uuid,
    #[serde(borrow)]
    pub event: BuildTaskEventKind<'a>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToOwned, IntoOwned)]
#[serde(rename_all = "snake_case")]
pub enum BuildTaskEventKind<'a> {
    Started(models::BuildTask<'a>),
    Stopped,
    #[serde(borrow)]
    Updated(BuildTaskUpdate<'a>),
    #[serde(borrow)]
    Log(Log<'a>),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToOwned, IntoOwned)]
#[serde(rename_all = "snake_case")]
pub enum BuildTaskUpdate<'a> {
    FileLinked {
        bytes: u32,
    },
    UntrustedPath,
    CorruptedPath,
    SetPhase {
        phase: Cow<'a, str>,
    },
    Progress {
        done: u32,
        expected: u32,
        running: u32,
        failed: u32,
    },
}

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
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    #[default]
    Build,
    PostBuild,
    Message,
}
impl LogKind {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct Log<'a> {
    pub level: models::BuildTaskLevel,
    #[serde(skip_serializing_if = "LogKind::is_default", default)]
    pub kind: LogKind,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub file: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
    pub text: Cow<'a, str>,
}
