// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use gorgond_client::builds;
use miette::Diagnostic;
use std::{borrow::Cow, collections::BTreeMap, ffi::OsStr, fmt, path::Path, process::Command};
use tracing::{debug, instrument};

#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum HelperError {
    #[error("IO Error starting helper process")]
    Start(#[source] std::io::Error),
    #[error(fmt = Self::fmt_exit_status)]
    Exit(Option<i32>),
}

impl HelperError {
    fn fmt_exit_status(status: &Option<i32>, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Helper process exited with status: ")?;
        match status {
            Some(v) => write!(f, "{v}"),
            None => write!(f, "???"),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Helper<'a> {
    pub name: Cow<'a, str>,
    pub command: Cow<'a, Path>,
    pub args: Vec<Cow<'a, str>>,
    pub current_dir: Option<Cow<'a, Path>>,
}

impl Helper<'_> {
    pub fn run(&self) -> Result<(), HelperError> {
        let mut cmd = Command::new(&*self.command);
        cmd.args(self.args.iter().map(|s| s.as_ref()));
        if let Some(path) = self.current_dir.as_ref() {
            cmd.current_dir(path);
        };

        let status = cmd.status().map_err(HelperError::Start)?;
        if !status.success() {
            return Err(HelperError::Exit(status.code()));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum FetcherError {
    #[error("Fetcher not configured: '{0}'")]
    Unconfigured(String),
}

#[instrument(skip_all)]
pub fn fetcher<'a>(
    fetchers: &'a BTreeMap<String, crate::FetcherConfig>,
    src: &'a builds::Source<'a>,
) -> Result<Helper<'a>, FetcherError> {
    let cfg = fetchers
        .get(src.kind.as_ref())
        .ok_or_else(|| FetcherError::Unconfigured(src.kind.to_string()))?;
    let mut args = Vec::<Cow<'a, str>>::from_iter(["--url".into(), src.url.as_str().into()]);
    if let Some(rev) = src.rev.as_ref() {
        args.extend_from_slice(&["--rev".into(), rev.as_ref().into()]);
    }
    Ok(Helper {
        name: src.kind.clone(),
        command: cfg.command.as_path().into(),
        args,
        current_dir: None,
    })
}

#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum BuilderError {
    #[error("Builder not configured: '{0}'")]
    Unconfigured(String),
}

#[instrument(skip_all)]
pub fn builder<'a>(
    builders: &'a BTreeMap<String, crate::BuilderConfig>,
    build: &'a builds::Build<'static>,
) -> Result<Helper<'a>, BuilderError> {
    let cfg = builders
        .get(build.kind.as_ref())
        .ok_or_else(|| BuilderError::Unconfigured(build.kind.to_string()))?;
    Ok(Helper {
        name: build.kind.clone(),
        command: cfg.command.as_path().into(),
        args: cfg.args.iter().map(|s| s.as_str().into()).collect(),
        current_dir: cfg.current_dir.as_deref().map(|v| v.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use test_log::test;
    use url::Url;

    #[test]
    fn test_fetcher_for_git() {
        let cfg = Config::default();
        let src = builds::Source {
            name: "test-build".into(),
            kind: "git".into(),
            rev: Some("main".into()),
            url: Url::parse("https://example.com/").unwrap(),
        };
        let fetcher = fetcher(&cfg.fetchers, &src).unwrap();
        assert_eq!(
            Helper {
                name: "git".into(),
                command: PathBuf::from("gorgon-build-fetch-git").into(),
                args: ["--url", "https://example.com/", "--rev", "main"]
                    .into_iter()
                    .map(|v| v.into())
                    .collect(),
                current_dir: None,
            },
            fetcher
        );
    }
    #[test]
    fn test_fetcher_for_unconfigured() {
        let err = fetcher(
            &Config::default().fetchers,
            &builds::Source {
                name: "test-build".into(),
                kind: "cvs".into(),
                rev: Some("main".into()),
                url: Url::parse("https://example.com/").unwrap(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, FetcherError::Unconfigured(name) if name == "cvs"));
    }

    #[test]
    fn test_builder_for_nix() {
        let cfg = Config::default();
        let build = builds::Build {
            id: gorgond_client::Uuid::nil(),
            name: "test-build".into(),
            kind: "nix".into(),
            source: builds::Source {
                name: "test-build".into(),
                kind: "git".into(),
                rev: Some("main".into()),
                url: Url::parse("https://example.com/").unwrap(),
            },
        };
        let fetcher = builder(&cfg.builders, &build).unwrap();
        assert_eq!(
            Helper {
                name: "nix".into(),
                command: PathBuf::from("gorgon-build-nix").into(),
                args: vec![],
                current_dir: None,
            },
            fetcher
        );
    }
    #[test]
    fn test_builder_for_unconfigured() {
        let err = builder(
            &Config::default().builders,
            &builds::Build {
                id: gorgond_client::Uuid::nil(),
                name: "test-build".into(),
                kind: "make".into(),
                source: builds::Source {
                    name: "test-build".into(),
                    kind: "git".into(),
                    rev: Some("main".into()),
                    url: Url::parse("https://example.com/").unwrap(),
                },
            },
        )
        .unwrap_err();
        assert!(matches!(err, BuilderError::Unconfigured(name) if name == "make"));
    }

    #[test]
    fn test_run_helper() {
        let tmpdir = tempfile::tempdir().expect("Couldn't create temporary directory");
        let helper = Helper {
            name: "sh".into(),
            command: which::which("sh").expect("Couldn't find `sh`").into(),
            args: ["-c", "echo 'hello' > test.txt"]
                .into_iter()
                .map(|v| v.into())
                .collect(),
            current_dir: Some(tmpdir.path().into()),
        };
        helper.run().expect("Couldn't run helper");

        let contents = std::fs::read_to_string(tmpdir.path().join("test.txt"))
            .expect("Couldn't read expected output");
        assert_eq!(contents, "hello\n".to_string());
    }
}
