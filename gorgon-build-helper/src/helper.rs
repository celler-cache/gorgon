// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{
    borrow::Cow,
    cell::LazyCell,
    ffi::OsStr,
    fmt,
    io::BufRead,
    path::Path,
    process::{Command, Stdio},
};

use bounded_static::IntoBoundedStatic;
use log::{debug, error, info, log_enabled, trace, warn};
use miette::{NamedSource, SourceOffset};
use owo_colors::OwoColorize;

use crate::{events::Event, task::Task, SerializedError};

pub type Result<T, E = HelperError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum HelperError {
    #[error(fmt = Self::fmt_find_binary)]
    FindBinary(String, Vec<String>, #[source] which::Error),

    #[error("Failed to start helper process")]
    Start(#[source] std::io::Error),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Exit(HelperExitError),
}

impl HelperError {
    fn fmt_find_binary(
        name: &String,
        paths: &Vec<String>,
        _: &which::Error,
        f: &mut fmt::Formatter,
    ) -> fmt::Result {
        write!(f, "Looking for `{name}`, in:")?;
        for path in paths {
            write!(f, "\n  - {path}")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub struct HelperExitError {
    pub name: String,
    pub status: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

impl fmt::Display for HelperExitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = &self.name;
        match (self.stdout.as_ref(), self.stderr.as_ref()) {
            (Some(s), None) | (None, Some(s)) if !s.contains("\n") => {
                write!(f, "{name}: {s}")?;
            }
            (Some(s), None) => write!(f, "STDOUT:\n\n{s}\n\n")?,
            (None, Some(s)) => write!(f, "STDERR:\n\n{s}\n\n")?,
            (Some(s1), Some(s2)) => {
                write!(f, "STDOUT:\n\n{s1}\n\n")?;
                write!(f, "STDERR:\n\n{s2}\n\n")?;
            }
            (None, None) => match self.status {
                Some(v) => write!(f, "{name} exited with status code: {v}")?,
                None => write!(f, "{name} exited with unknown status")?,
            },
        };
        Ok(())
    }
}

impl miette::Diagnostic for HelperExitError {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.status
            .map(|status| Box::new(format!("{} failed with code: {status}", self.name)) as _)
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum TaskError {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Helper(#[from] HelperError),
    #[error("Couldn't deserialize task error")]
    #[diagnostic(transparent)]
    ErrorJson(#[from] JsonError),
    #[error("Couldn't process stderr")]
    #[diagnostic(transparent)]
    Stderr(#[from] StderrError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Task(#[from] TaskExitError),
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("Task failed: `{}`", .1.name)]
pub struct TaskExitError(#[related] Vec<TaskExitErrorCause>, Task<'static>);

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("Error from task: `{}`", .1)]
#[diagnostic(forward(0))]
struct TaskExitErrorCause(#[source] SerializedError, Cow<'static, str>);

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum StderrError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Couldn't deserialize event")]
    #[diagnostic(transparent)]
    EventJson(#[from] JsonError),
    #[error("Couldn't send deserialized error back to main thread")]
    SendError(#[from] crossbeam::channel::TrySendError<SerializedError>),
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{err}")]
pub struct JsonError {
    pub err: serde_json::Error,
    #[source_code]
    pub src: NamedSource<String>,
    #[label("{err}")]
    pub pos: SourceOffset,
}

impl JsonError {
    pub fn new(src: NamedSource<String>, err: serde_json::Error) -> Self {
        Self {
            pos: SourceOffset::from_location(src.inner(), err.line(), err.column()),
            src,
            err,
        }
    }
}

pub trait HelperContext {
    fn apply(&self, helper: Helper) -> Helper;
}
impl HelperContext for () {
    fn apply(&self, helper: Helper) -> Helper {
        helper
    }
}

pub trait HelperCaller: Sized {
    fn helper(&self, name: impl AsRef<OsStr>) -> Result<Helper>;
    fn with<Ctx: HelperContext>(self, ctx: Ctx) -> impl HelperCaller {
        HelperCallerWith(ctx, self)
    }
}

pub struct HelperFinder<'a> {
    pub current_dir: Cow<'a, Path>,
    pub path: Cow<'a, OsStr>,
    pub dev: bool,
    pub dev_repo: Option<Cow<'a, Path>>,
}

impl<'a> HelperFinder<'a> {
    pub fn new(current_dir: Cow<'a, Path>, path: Cow<'a, OsStr>) -> Self {
        Self {
            current_dir,
            path,
            dev: false,
            dev_repo: None,
        }
    }
    pub fn with_dev(&mut self, dev_repo: Option<Cow<'a, Path>>) -> &mut Self {
        self.dev = true;
        self.dev_repo = dev_repo;
        self
    }
}
impl HelperCaller for HelperFinder<'_> {
    fn helper(&self, name: impl AsRef<OsStr>) -> Result<Helper> {
        if self.dev {
            let repo_path = self.dev_repo.as_ref().unwrap_or(&self.current_dir);
            let src_path = repo_path.join(Path::new(&name));
            match src_path.try_exists() {
                Ok(true) => {
                    info!(
                        target: "helper::dev", name=name.as_ref().to_string_lossy();
                        "🚧 Building helper from source..."
                    );
                    return Helper::build_dev(&name, repo_path, &self.current_dir);
                }
                Ok(false) => trace!(target: "helper::dev", src_path:?; "Path does not exist"),
                Err(err) => {
                    debug!(target: "helper::dev", src_path:?, err:%; "Couldn't check if path exists")
                }
            }
        }

        Helper::find(name, &self.path, &self.current_dir)
    }
}

pub struct HelperCallerWith<Ctx: HelperContext, C: HelperCaller>(Ctx, C);

impl<Ctx: HelperContext, C: HelperCaller> HelperCaller for HelperCallerWith<Ctx, C> {
    fn helper(&self, name: impl AsRef<OsStr>) -> Result<Helper> {
        self.1.helper(name).map(|h| h.with(&self.0))
    }
}

#[derive(Debug)]
pub struct Helper {
    pub cmd: Command,
}

impl From<Command> for Helper {
    fn from(cmd: Command) -> Self {
        Self { cmd }
    }
}

impl fmt::Display for Helper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (key, value) in self.cmd.get_envs() {
            let key = key.to_string_lossy();
            let value = value.map(|s| s.to_string_lossy()).unwrap_or_default();
            writeln!(f, "{key}={value}")?;
        }
        let cwd = self
            .cmd
            .get_current_dir()
            .map(|p| p.to_string_lossy())
            .unwrap_or_default();
        writeln!(f, "[{cwd}]")?;
        write!(f, "$ {}", self.cmd.get_program().to_string_lossy())?;
        for arg in self.cmd.get_args() {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        Ok(())
    }
}

impl Helper {
    fn name(&self) -> Cow<'_, str> {
        let path = Path::new(self.cmd.get_program());
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
    }

    pub fn run(mut self) -> Result<()> {
        if log_enabled!(log::Level::Debug) {
            debug!(target:"helper", __log__:display=self; "Invoking helper...");
        }

        let output = self.cmd.output().map_err(HelperError::Start)?;
        match output.status {
            status if !status.success() => Err(HelperError::Exit(HelperExitError {
                name: self.name().to_string(),
                status: status.code(),
                stdout: Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
                    .filter(|s| !s.is_empty()),
                stderr: Some(String::from_utf8_lossy(&output.stderr).trim().to_string())
                    .filter(|s| !s.is_empty()),
            })),
            _ => Ok(()),
        }
    }

    pub fn build_dev(
        name: impl AsRef<OsStr>,
        repo_path: impl AsRef<Path>,
        cwd: impl AsRef<Path>,
    ) -> Result<Self> {
        Self::from(Command::new("cargo"))
            .with_args(["build", "-p"])
            .with_arg(name.as_ref())
            .with_cwd(repo_path.as_ref())
            .run()?;
        Self::find(name, repo_path.as_ref().join("target/debug/"), cwd)
    }

    pub fn find(
        name: impl AsRef<OsStr>,
        paths: impl AsRef<OsStr>,
        cwd: impl AsRef<Path>,
    ) -> Result<Self> {
        Ok(Self::from(Command::new(
            which::which_in(name.as_ref(), Some(paths.as_ref()), cwd).map_err(|err| {
                HelperError::FindBinary(
                    name.as_ref().to_string_lossy().into(),
                    std::env::split_paths(&paths)
                        .map(|p| p.to_string_lossy().to_string())
                        .collect(),
                    err,
                )
            })?,
        )))
    }

    pub fn with<C: HelperContext>(self, ctx: &C) -> Self {
        ctx.apply(self)
    }
    pub fn with_env(mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> Self {
        self.cmd.env(key, val);
        self
    }
    pub fn with_arg(mut self, p: impl AsRef<OsStr>) -> Self {
        self.cmd.arg(p);
        self
    }
    pub fn with_arg_opt(mut self, p: Option<impl AsRef<OsStr>>) -> Self {
        if let Some(p) = p {
            self.cmd.arg(p);
        }
        self
    }
    pub fn with_args(mut self, it: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Self {
        self.cmd.args(it);
        self
    }
    pub fn with_cwd(mut self, cwd: impl AsRef<Path>) -> Self {
        self.cmd.current_dir(cwd);
        self
    }

    pub fn into_task(self) -> TaskHelper<'static> {
        TaskHelper::from(self)
    }
}

pub struct TaskHelper<'a> {
    task: Task<'a>,
    helper: Helper,
}

impl From<Helper> for TaskHelper<'static> {
    fn from(helper: Helper) -> Self {
        Self {
            task: Task::new(helper.name()).into_static(),
            helper,
        }
    }
}

impl TaskHelper<'_> {
    pub fn run(self) -> Result<(), TaskError> {
        if log_enabled!(log::Level::Debug) {
            debug!(target:"helper::task", __log__:display=self.helper; "Invoking task helper...");
        }

        let mut child = self
            .helper
            .with_env("GORGON_VERBOSE", "100")
            .with_env("GORGON_OUTPUT_FORMAT", "json")
            .with_env("GORGON_LOG_FORMAT", "json")
            .cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(HelperError::Start)?;

        let (send_err, recv_err) = crossbeam::channel::unbounded::<SerializedError>();
        let join_stderr = std::thread::spawn({
            let name = self.task.name.clone().into_owned();
            let mut stderr = std::io::BufReader::new(child.stderr.take().expect("No child stderr"));
            move || -> Result<(), StderrError> {
                let mut buf = String::new();
                loop {
                    stderr.read_line(&mut buf)?;
                    if buf.is_empty() {
                        return Ok(());
                    }
                    if buf.starts_with('{') {
                        let event: Event = match serde_json::from_str(&buf) {
                            Err(err) if err.is_eof() => {
                                // Keep reading lines until we've got a complete JSON payload.
                                continue;
                            }
                            Err(err) => Err(StderrError::from(JsonError::new(
                                NamedSource::new("stderr", buf.clone()).with_language("JSON"),
                                err,
                            ))),
                            Ok(event) => Ok(event),
                        }?;
                        match event {
                            Event::Error(err) => send_err.try_send(err)?,
                            Event::Log(log) => {
                                let target = format!("`{name}`::{}", log.target);
                                let meta = log::Metadata::builder()
                                    .level(log.level)
                                    .target(&target)
                                    .build();
                                let logger = log::logger();
                                if logger.enabled(&meta) {
                                    logger.log(
                                        &log::Record::builder()
                                            .metadata(meta)
                                            .args(format_args!("{}", log.msg))
                                            .key_values(&log)
                                            .module_path(log.module_path.as_deref())
                                            .file(log.file.as_deref())
                                            .line(log.line)
                                            .build(),
                                    );
                                }
                            }
                        }
                    } else {
                        warn!("stderr: {}", buf.trim_end());
                    }

                    // Now we can clear the buffer.
                    buf.clear();
                }
            }
        });

        let output = child.wait_with_output().map_err(HelperError::Start)?;
        join_stderr.join().expect("stderr thread panicked!?")?;
        match output.status {
            status if !status.success() => {
                let stdout_str = LazyCell::new(|| String::from_utf8_lossy(&output.stdout));

                // If debug logging is enabled, log stdout.
                if log_enabled!(log::Level::Debug) {
                    let termwidth = terminal_size::terminal_size_of(std::io::stderr())
                        .map(|(w, _)| w.0 as usize)
                        .unwrap_or(80);
                    eprintln!(
                        "{}",
                        format!(
                            "{:-^termwidth$}\n{}{:-^termwidth$}",
                            " DEBUG --- failed task stdout --- DEBUG ",
                            stdout_str.as_ref(),
                            ""
                        )
                        .dimmed()
                    );
                }

                // Try to parse the forwarded error from the child.
                let task = self.task.into_static();
                Err(TaskError::Task(TaskExitError(
                    recv_err
                        .into_iter()
                        .map(|r| TaskExitErrorCause(r, task.name.clone()))
                        .collect(),
                    task,
                )))
            }
            _ => {
                if let Some(err) = recv_err.into_iter().next() {
                    unimplemented!("non-fatal error from child: {err}");
                }
                Ok(())
            }
        }
    }
}
