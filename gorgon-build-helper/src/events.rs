// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{borrow::Cow, collections::BTreeMap};

use gorgond_client::Uuid;
use log::kv::ToKey;
use serde::{Deserialize, Serialize};
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};

static mut HANDLER: &dyn EventHandler = &();

/// # SAFETY
/// This is safe to call up until events start being produced.
pub unsafe fn set_handler(handler: &'static dyn EventHandler) {
    HANDLER = handler
}
pub fn handler() -> &'static dyn EventHandler {
    // SAFETY: This is only ever assigned by the set_handler() function.
    unsafe { HANDLER }
}

pub trait EventHandler {
    fn handle(&self, target: &str, event: Event);
}
impl EventHandler for () {
    fn handle(&self, _: &str, _: Event) {}
}

/// Task Verbosity, matching nix' verbosity levels.
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
)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Unknown,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event<'a> {
    #[serde(borrow)]
    Task(TaskEvent<'a>),
    #[serde(borrow)]
    Log(LogEvent<'a>),
    Error(crate::SerializedError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TaskEvent<'a> {
    #[serde(rename = "tid")]
    pub task_sid: Uuid,
    #[serde(rename = "eid")]
    pub event_sid: Uuid,
    #[serde(borrow, flatten)]
    pub event: TaskEventKind<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind<'a> {
    #[serde(borrow)]
    Start(TaskStart<'a>),
    Stop,
    Log(TaskLog<'a>),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TaskStart<'a> {
    #[serde(rename = "pid", skip_serializing_if = "Option::is_none", default)]
    pub parent_sid: Option<Uuid>,
    #[serde(rename = "lvl")]
    pub level: TaskLevel,
    pub kind: TaskKind,
    pub name: Cow<'a, str>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TaskLog<'a> {
    pub level: Option<TaskLevel>,
    #[serde(skip_serializing_if = "TaskLogKind::is_default", default)]
    pub kind: TaskLogKind,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub file: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
    pub log: Cow<'a, str>,
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
)]
#[serde(rename_all = "snake_case")]
pub enum TaskLogKind {
    #[default]
    Build,
    PostBuild,
    Message,
}
impl TaskLogKind {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LogEvent<'a> {
    #[serde(rename = "lvl")]
    pub level: log::Level,
    #[serde(borrow, rename = "tgt")]
    pub target: Cow<'a, str>,
    #[serde(borrow, rename = "msg")]
    pub msg: Cow<'a, str>,
    #[serde(
        borrow,
        rename = "kv",
        skip_serializing_if = "BTreeMap::is_empty",
        default
    )]
    pub key_values: BTreeMap<Cow<'a, str>, serde_json::Value>,
    #[serde(
        borrow,
        rename = "mod",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub module_path: Option<Cow<'a, str>>,
    #[serde(
        borrow,
        rename = "file",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub file: Option<Cow<'a, str>>,
    #[serde(rename = "line", skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
}
impl LogEvent<'_> {
    pub fn log(self, target: &str) {
        let target: Cow<'_, str> = match Some(&*self.target).filter(|s| !s.is_empty()) {
            Some(t) => Cow::Owned(format!("{target}::{}", t)),
            None => Cow::Borrowed(target),
        };
        let meta = log::Metadata::builder()
            .level(self.level)
            .target(&target)
            .build();
        let logger = log::logger();
        if logger.enabled(&meta) {
            logger.log(
                &log::Record::builder()
                    .metadata(meta)
                    .args(format_args!("{}", self.msg))
                    .key_values(&self)
                    .module_path(self.module_path.as_deref())
                    .file(self.file.as_deref())
                    .line(self.line)
                    .build(),
            );
        }
    }
}

impl<'a> TryFrom<&'a log::Record<'a>> for LogEvent<'a> {
    type Error = log::kv::Error;
    fn try_from(rec: &'a log::Record<'a>) -> Result<Self, Self::Error> {
        #[derive(Default)]
        struct Visitor<'a>(BTreeMap<Cow<'a, str>, serde_json::Value>);
        impl<'a> log::kv::VisitSource<'a> for Visitor<'a> {
            fn visit_pair(
                &mut self,
                key: log::kv::Key<'a>,
                value: log::kv::Value<'a>,
            ) -> Result<(), log::kv::Error> {
                self.0.insert(
                    key.to_borrowed_str()
                        .map(Cow::Borrowed)
                        .unwrap_or_else(|| Cow::Owned(key.as_str().into())),
                    serde_json::to_value(value).map_err(log::kv::Error::boxed)?,
                );
                Ok(())
            }
        }
        let key_values = {
            let mut visitor = Visitor::default();
            rec.key_values().visit(&mut visitor)?;
            visitor.0
        };

        let args = rec.args();
        Ok(Self {
            level: rec.level(),
            target: rec.target().into(),
            msg: args
                .as_str()
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Owned(format!("{args}"))),
            key_values,
            module_path: rec.module_path().map(|s| s.into()),
            file: rec.file().map(|s| s.into()),
            line: rec.line(),
        })
    }
}

impl log::kv::Source for LogEvent<'_> {
    fn visit<'kvs>(
        &'kvs self,
        visitor: &mut dyn log::kv::VisitSource<'kvs>,
    ) -> Result<(), log::kv::Error> {
        for (k, v) in self.key_values.iter() {
            visitor.visit_pair(k.to_key(), log::kv::Value::from_serde(v))?;
        }
        Ok(())
    }
}
