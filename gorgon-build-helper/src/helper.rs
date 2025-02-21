// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{ffi::OsStr, fmt, path::Path, process::Command};

use owo_colors::OwoColorize;
use tracing::{debug, event_enabled};

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
