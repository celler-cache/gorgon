// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{borrow::Cow, ffi::OsStr, fmt, path::Path, process::Command};

use owo_colors::OwoColorize;
use tracing::{debug, debug_span, event_enabled, instrument};

pub type Result<T, E = HelperError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum HelperError {
    #[error(fmt = Self::fmt_find_binary)]
    FindBinary(String, Vec<String>, #[source] which::Error),

    #[error("IO Error starting helper process")]
    Start(#[source] std::io::Error),
    #[error(fmt = Self::fmt_exit_status)]
    Exit(Option<i32>),
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

    fn fmt_exit_status(status: &Option<i32>, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Helper process exited with status: ")?;
        match status {
            Some(v) => write!(f, "{v}"),
            None => write!(f, "[unknown]"),
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
    #[instrument(name="finder", skip_all, fields(name=?name.as_ref()))]
    fn helper(&self, name: impl AsRef<OsStr>) -> Result<Helper> {
        if self.dev {
            let _enter = debug_span!("dev").entered();
            let repo_path = self.dev_repo.as_ref().unwrap_or(&self.current_dir);
            let src_path = repo_path.join(Path::new(&name));
            match src_path.try_exists() {
                Ok(true) => return Helper::build_dev(&name, repo_path, &self.current_dir),
                Ok(false) => debug!(?src_path, "Path does not exist"),
                Err(err) => debug!(?src_path, %err, "Couldn't check if path exists"),
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

pub struct Helper {
    cmd: Command,
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
    pub fn run(mut self) -> Result<()> {
        let _enter = tracing::debug_span!("helper", cmd = ?self.cmd.get_program()).entered();
        if event_enabled!(tracing::Level::DEBUG) {
            debug!("Invoking helper:\n{self}");
        }

        let status = {
            // Print a header.
            let termwidth = terminal_size::terminal_size_of(std::io::stderr())
                .map(|(w, _)| w.0 as usize)
                .unwrap_or(80);
            let divider_style = owo_colors::style().dimmed();
            let path = self.cmd.get_program();
            let name = Path::new(path)
                .file_name()
                .unwrap_or(path)
                .to_string_lossy();
            eprintln!("{}", format!("{name:-^termwidth$}").style(divider_style));

            // This bit runs the actual command.
            let r = self.cmd.status().map_err(HelperError::Start);

            // Print a footer.
            eprintln!("{}", format!("{:-^termwidth$}", "").style(divider_style));

            r?
        };

        match status {
            status if !status.success() => Err(HelperError::Exit(status.code())),
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
}

impl From<Command> for Helper {
    fn from(cmd: Command) -> Self {
        Self { cmd }
    }
}
