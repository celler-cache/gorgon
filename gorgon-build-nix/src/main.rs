// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use clap::Parser;
use clap_stdin::MaybeStdin;
use derive_more::{Deref, DerefMut};
use gorgon_build_helper::{helper::Helper, task::Task, Uuid};
use gorgond_client::builds::Build;
use log::{
    info,
    kv::{Key, Value},
    warn,
};
use miette::{miette, Context, IntoDiagnostic, Result};
use nix_daemon::{
    nix::internal_json::{
        Action, ActionMessage, ActionResult, ActionResultData, ActionStart, ActionStop,
        BuildLogLine, FileLinked, Progress, SetExpected, SetPhase,
    },
    Verbosity,
};
use once_cell::unsync::Lazy;
use std::{
    cell::LazyCell,
    collections::BTreeMap,
    fmt::Write,
    io::{BufRead, BufReader},
    ops::Deref,
    path::{Path, PathBuf},
    process::Stdio,
};
use url::{Position, Url};

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Path to `nix-instantiate`.
    #[arg(long, short = 'I', default_value = "nix-instantiate")]
    nix_instantiate: PathBuf,

    /// Path to `nix-store`.
    #[arg(long, short = 'S', default_value = "nix-store")]
    nix_store: PathBuf,

    /// Build spec in JSON format, or "-" to read from stdin.
    build_json: MaybeStdin<String>,

    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error, miette::Diagnostic)]
pub enum FetchExprError {
    #[error("{1}")]
    #[diagnostic(code = "FetchExpr::url::parse")]
    ParseUrl(String, #[source] url::ParseError),
    #[error("{1}")]
    #[diagnostic(forward(1))]
    Url(Url, #[source] FetchExprUrlError),
}
#[derive(Debug, PartialEq, Eq, thiserror::Error, miette::Diagnostic)]
pub enum FetchExprUrlError {
    #[error("Unsupported fetcher: {0}")]
    #[diagnostic(
        code = "FetchExpr::url::scheme",
        help = "Fetchers are specified by the URL scheme, eg. 'git://' or 'git+http://'.\nSupported fetchers: 'git'."
    )]
    Fetcher(String),
    #[error("Unknown query parameter: ?{0}")]
    #[diagnostic(
        code = "FetchExpr::url::query::unknown",
        help = "Valid query parameters are: ?rev"
    )]
    UnknownQueryParameter(String),
    #[error("Missing required query parameter: ?{0}")]
    #[diagnostic(
        code = "FetchExpr::url::query::required",
        help = "Required query parameters are: ?rev"
    )]
    MissingQueryParameter(&'static str),
}

/// A nix expression for a fetcher, eg. `builtins.fetchGit`.
#[derive(Debug, PartialEq, Eq)]
pub enum FetchExpr {
    FetchGit { url: String, rev: String },
}
impl std::fmt::Display for FetchExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FetchGit { url, rev } => write!(
                f,
                r#"builtins.fetchGit {{ url = "{url}"; rev = "{rev}"; }}"#,
            ),
        }
    }
}
impl FetchExpr {
    /// Parses a Fetcher URL, eg. `git+https://example.com/?rev=abc123`.
    pub fn parse_url(url_s: &str) -> Result<Self, FetchExprError> {
        let url = Url::parse(url_s).map_err(|err| FetchExprError::ParseUrl(url_s.into(), err))?;
        let url_err = |err| FetchExprError::Url(url.clone(), err);

        // The URL crate won't let us do `set_scheme()` involving "special" schemes like
        // https://, which... is unfortunate for doing git+https -> https, so if we need
        // to do that, we need to rebuild the whole string ourselves.
        // https://github.com/servo/rust-url/issues/577
        let scheme_ = url.scheme();
        let (fetcher, scheme) = scheme_.split_once("+").unwrap_or((scheme_, scheme_));
        let url_s = [scheme, &url[Position::AfterScheme..Position::AfterPath]].join("");

        match fetcher {
            "git" => {
                let mut rev = None;
                for (k, v) in url.query_pairs() {
                    match k.deref() {
                        "rev" => rev = Some(v.to_string()),
                        _ => {
                            return Err(url_err(FetchExprUrlError::UnknownQueryParameter(k.into())))
                        }
                    }
                }
                Ok(Self::FetchGit {
                    url: url_s,
                    rev: rev
                        .ok_or_else(|| url_err(FetchExprUrlError::MissingQueryParameter("rev")))?,
                })
            }
            _ => Err(url_err(FetchExprUrlError::Fetcher(fetcher.into()))),
        }
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum TaskError {
    #[error("Task not found: {0}")]
    NotFound(Uuid),
    #[error("Attempted to register a duplicate task:\nNew: {new:?}\nOld: {old:?}")]
    Duplicate { new: Box<Task>, old: Box<Task> },
}

#[derive(Debug, Default)]
struct Tasks {
    pub tasks: BTreeMap<Uuid, Task>,
}
impl Tasks {
    pub fn start(&mut self, task: Task) -> Result<&Task, TaskError> {
        if let Some(old) = self.tasks.get(&task.sid) {
            Err(TaskError::Duplicate {
                new: Box::new(task.clone()),
                old: Box::new(old.clone()),
            })
        } else {
            let sid = task.sid;
            self.tasks.insert(sid, task);
            Ok(self
                .tasks
                .get(&sid)
                .expect("can't get task we just inserted?"))
        }
    }

    pub fn try_get(&self, sid: Uuid) -> Result<&Task, TaskError> {
        self.tasks.get(&sid).ok_or(TaskError::NotFound(sid))
    }
    pub fn try_get_mut(&mut self, sid: Uuid) -> Result<&mut Task, TaskError> {
        self.tasks.get_mut(&sid).ok_or(TaskError::NotFound(sid))
    }
    pub fn log_target<'a>(
        &self,
        buf: &'a mut String,
        prefix: impl std::fmt::Display,
        sid: Uuid,
    ) -> Result<&'a str, TaskError> {
        fn walk_log_target<'t>(
            tasks: &'t Tasks,
            buf: &mut String,
            sid: Uuid,
        ) -> Result<&'t Task, TaskError> {
            let task = tasks.try_get(sid)?;
            if let Some(parent_sid) = task.parent_sid {
                walk_log_target(tasks, buf, parent_sid)?;
            }
            if !task.name.is_empty() {
                write!(buf, "::{}", task.name).unwrap();
            }
            Ok(task)
        }
        buf.clear();
        write!(buf, "{}", prefix).unwrap();
        let task = walk_log_target(self, buf, sid)?;
        if let Some(phase) = &task.phase {
            write!(buf, "::{phase}").unwrap();
        }
        Ok(buf)
    }
}

#[derive(Debug, Deref, DerefMut)]
struct NixCommand(Helper);

impl NixCommand {
    fn new(path: impl AsRef<Path>) -> Self {
        let mut helper = Helper::new(path.as_ref());
        helper
            .args(["--log-format", "internal-json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self(helper)
    }

    fn input(&mut self, name: &str, expr: &FetchExpr) {
        self.args(["--arg", name, &expr.to_string()]);
    }
    fn input_url(&mut self, name: &str, url_s: &str) -> Result<(), FetchExprError> {
        self.input(name, &FetchExpr::parse_url(url_s)?);
        Ok(())
    }

    fn call(mut self) -> Result<String> {
        let mut child = self.start()?;

        let join_stderr = std::thread::spawn({
            let stderr = child.child.stderr.take().expect("child has no stderr");
            move || -> Result<()> {
                let mut buf = String::new();
                let mut tasks = Tasks::default();

                let mut lines = BufReader::new(stderr).lines();
                while let Some(line) = lines.next().transpose().into_diagnostic()? {
                    if let Some(json) = line.strip_prefix("@nix ") {
                        let logger = log::logger();
                        let action = match serde_json::from_str::<Action>(json).into_diagnostic() {
                            Ok(action) => action,
                            Err(err) => {
                                warn!(err:display; "Couldn't decode log line");
                                continue;
                            }
                        };
                        match action {
                            Action::Start(ActionStart {
                                id,
                                level,
                                parent,
                                text,
                                type_,
                            }) => {
                                let sid = activity_sid(id);
                                tasks.start(Task {
                                    sid,
                                    parent_sid: Some(parent)
                                        .filter(|id| *id != 0)
                                        .map(activity_sid),
                                    name: text.clone().into(),
                                    level: Some(nix_level(level)),
                                    ..Default::default()
                                })?;
                                let metadata = log::Metadata::builder()
                                    .target(tasks.log_target(&mut buf, "nix", sid)?)
                                    .level(log::Level::Trace)
                                    .build();
                                if logger.enabled(&metadata) {
                                    logger.log(
                                        &log::Record::builder()
                                            .metadata(metadata)
                                            .key_values(&[
                                                ("id", Value::from(id)),
                                                ("type", Value::from_debug(&type_)),
                                            ])
                                            .args(format_args!("Started"))
                                            .build(),
                                    );
                                }
                            }
                            Action::Stop(ActionStop { id }) => {
                                let sid = activity_sid(id);
                                let metadata = log::Metadata::builder()
                                    .target(tasks.log_target(&mut buf, "nix", sid)?)
                                    .level(log::Level::Trace)
                                    .build();
                                if logger.enabled(&metadata) {
                                    logger.log(
                                        &log::Record::builder()
                                            .metadata(metadata)
                                            .key_values(&[("id", Value::from(id))])
                                            .args(format_args!("Stopped"))
                                            .build(),
                                    );
                                }
                            }
                            Action::Result(ActionResult { id, data }) => {
                                let sid = activity_sid(id);
                                let target = tasks.log_target(&mut buf, "nix", sid)?;
                                let task = tasks.try_get_mut(sid)?;
                                let metadata = log::Metadata::builder()
                                    .level(log::Level::Trace)
                                    .target(target)
                                    .build();
                                if logger.enabled(&metadata) {
                                    let mut kv = BTreeMap::<&'static str, Value>::from_iter([(
                                        "id",
                                        id.into(),
                                    )]);
                                    let finish = move |kv, args| {
                                        logger.log(
                                            &log::Record::builder()
                                                .metadata(metadata)
                                                .key_values(kv)
                                                .args(args)
                                                .build(),
                                        )
                                    };

                                    match data {
                                        ActionResultData::FileLinked(FileLinked {
                                            bytes_linked,
                                        }) => {
                                            kv.insert("bytes_linked", bytes_linked.into());
                                            finish(&kv, format_args!("File Linked"))
                                        }
                                        ActionResultData::BuildLogLine(BuildLogLine { line })
                                        | ActionResultData::PostBuildLogLine(BuildLogLine {
                                            line,
                                        }) => finish(&kv, format_args!("{line}")),
                                        ActionResultData::UntrustedPath => {
                                            finish(&kv, format_args!("Untrusted Path"))
                                        }
                                        ActionResultData::CorruptedPath => {
                                            finish(&kv, format_args!("Corrupted Path"))
                                        }
                                        ActionResultData::SetPhase(SetPhase { phase }) => {
                                            task.phase = Some(phase);
                                            finish(&kv, format_args!("[ SetPhase ]"))
                                        }
                                        ActionResultData::Progress(Progress {
                                            done,
                                            expected,
                                            running,
                                            failed,
                                        }) => {
                                            kv.insert("done", done.into());
                                            kv.insert("expected", expected.into());
                                            kv.insert("running", running.into());
                                            kv.insert("failed", failed.into());
                                            finish(&kv, format_args!("[ Progress ]"))
                                        }
                                        ActionResultData::SetExpected(SetExpected {
                                            activity_type,
                                            expected,
                                        }) => {
                                            kv.insert(
                                                "activity_type",
                                                <&str>::from(activity_type).into(),
                                            );
                                            kv.insert("expected", expected.into());
                                            finish(&kv, format_args!("[ Set Expected ]"))
                                        }
                                    };
                                }
                            }
                            Action::Message(ActionMessage {
                                file,
                                column: _,
                                line,
                                level,
                                msg,
                            }) => {
                                let metadata = log::Metadata::builder()
                                    .level(nix_level(level))
                                    .target("nix")
                                    .build();
                                if logger.enabled(&metadata) {
                                    logger.log(
                                        &log::Record::builder()
                                            .metadata(metadata)
                                            .file(file.as_deref())
                                            .line(line.and_then(|v| v.try_into().ok()))
                                            .args(format_args!("{msg}"))
                                            .build(),
                                    )
                                }
                            }
                        }
                    }
                }
                Ok(())
            }
        });

        let out = {
            let out_r = child.wait_checked();
            join_stderr.join().expect("stderr thread panicked!?")?;
            out_r
        }?;
        if !out.status.success() {
            return match out.status.code() {
                Some(code) => Err(miette!("Process exited with status: {code}")),
                None => Err(miette!("Process exited with unknown status")),
            };
        }
        let mut stdout = String::from_utf8(out.stdout)
            .into_diagnostic()
            .context("stdout contains invalid UTF-8")?;
        stdout.truncate(stdout.trim_end().len());
        Ok(stdout)
    }
}

fn run(args: Args) -> Result<()> {
    let build: Build = serde_json::from_str(&args.build_json)
        .into_diagnostic()
        .context("Couldn't parse Build spec")?;

    // First, instantiate the derivation...
    let mut inst = NixCommand::new(args.nix_instantiate);
    for (i, (name, s)) in build.inputs.iter().enumerate() {
        inst.input_url(name, s)
            .with_context(|| format!("build.inputs[{i}]"))?;
    }
    inst.input_url("source", &build.source)
        .context("build.source")?;
    inst.args(["--expr", &build.expr]);
    info!(inst:?; "Instantiating derivation...");
    let drv = inst.call().context("nix-instantiate failed")?;

    // ...then realise (build) it.
    let mut build = NixCommand::new(args.nix_store);
    build.args(["--realise", &drv]);
    info!(build:?; "Building...");
    let out = build.call().context("nix-instantiate failed")?;

    println!("{out}");
    Ok(())
}

fn main() {
    let args = Args::parse();
    gorgon_util_cli::logger(args.common.log_format, args.common.level()).init();
    gorgon_util_cli::main(args.common.output_format, || run(args));
}

/// Casts a Nix Activity ID into a UUIDv8.
///
/// Nix (as of Lix 2.92) generates Activity IDs using:
///
/// ```cpp
/// std::atomic<uint64_t> nextId{0};
/// Activity::Activity(...): id(nextId++ + (((uint64_t) getpid()) << 32)) { ... }
/// ```
///
/// In other words, it's effectively a tuple of (pid: u32, id: u32) mashed together.
pub fn activity_sid(id: u64) -> Uuid {
    let id_bytes: [u8; 8] = id.to_be_bytes();
    let mut data = [0u8; 16];
    data[0..4].copy_from_slice(&id_bytes[0..4]);
    data[12..16].copy_from_slice(&id_bytes[4..8]);
    Uuid::new_v8(data)
}

pub fn nix_level(level: Verbosity) -> log::Level {
    match level {
        Verbosity::Error => log::Level::Error,
        Verbosity::Warn => log::Level::Warn,
        Verbosity::Notice | Verbosity::Info => log::Level::Info,
        Verbosity::Talkative | Verbosity::Chatty | Verbosity::Debug => log::Level::Debug,
        Verbosity::Vomit => log::Level::Trace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_fetchexpr_parse_url_bad_url() {
        assert_eq!(
            FetchExprError::ParseUrl("bad".into(), url::ParseError::RelativeUrlWithoutBase),
            FetchExpr::parse_url("bad").unwrap_err(),
        );
    }
    #[test]
    fn test_fetchexpr_parse_url_bad_scheme() {
        let url = "bad://";
        assert_eq!(
            FetchExprError::Url(
                Url::parse(url).unwrap(),
                FetchExprUrlError::Fetcher("bad".into()),
            ),
            FetchExpr::parse_url(url).unwrap_err(),
        );
    }
    #[test]
    fn test_fetchexpr_parse_url_unknown_query() {
        let url = "git+https://example.com/?bad=yes";
        assert_eq!(
            FetchExprError::Url(
                Url::parse(url).unwrap(),
                FetchExprUrlError::UnknownQueryParameter("bad".into()),
            ),
            FetchExpr::parse_url(url).unwrap_err(),
        );
    }
    #[test]
    fn test_fetchexpr_parse_url_missing_rev() {
        let url = "git+https://example.com/";
        assert_eq!(
            FetchExprError::Url(
                Url::parse(url).unwrap(),
                FetchExprUrlError::MissingQueryParameter("rev"),
            ),
            FetchExpr::parse_url(url).unwrap_err(),
        );
    }
    #[test]
    fn test_fetchexpr_parse_url_git() {
        assert_eq!(
            FetchExpr::FetchGit {
                url: "git://example.com/".into(),
                rev: "abc123".into(),
            },
            FetchExpr::parse_url("git://example.com/?rev=abc123").unwrap(),
        );
    }
    #[test]
    fn test_fetchexpr_parse_url_git_https() {
        assert_eq!(
            FetchExpr::FetchGit {
                url: "https://example.com/".into(),
                rev: "abc123".into(),
            },
            FetchExpr::parse_url("git+https://example.com/?rev=abc123").unwrap(),
        );
    }
}
