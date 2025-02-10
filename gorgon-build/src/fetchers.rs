// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use crate::FetcherConfig;
use gorgond_client::builds;
use miette::Diagnostic;
use std::{collections::BTreeMap, path::Path};
use tracing::instrument;

pub type Result<T, E = FetcherError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum FetcherError {
    #[error(transparent)]
    IO(#[from] std::io::Error),

    #[error("Unknown fetcher: {0}")]
    UnknownFetcher(String),
    #[error("Failed to spawn fetcher")]
    Spawn(#[source] std::io::Error),
    #[error("Fetcher exited with status: {}", .0.map(|s| s.to_string()).unwrap_or_else(|| "?".into()))]
    ExitStatus(Option<i32>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Fetcher<'a> {
    pub name: &'a str,
    pub cfg: &'a FetcherConfig,
    pub args: Vec<&'a str>,
}

impl std::fmt::Display for Fetcher<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dt = f.debug_tuple(self.name);
        for arg in self.args.iter() {
            dt.field(&arg);
        }
        dt.finish()
    }
}

impl<'a> Fetcher<'a> {
    #[instrument(skip_all)]
    pub fn for_source(
        fetchers: &'a BTreeMap<String, FetcherConfig>,
        src: &'a builds::Source<'a>,
    ) -> Result<Self> {
        let name = &src.kind;
        let cfg = fetchers
            .get(name.as_ref())
            .ok_or_else(|| FetcherError::UnknownFetcher(name.to_string()))?;
        let mut args = Vec::from_iter(["--url", src.url.as_str()]);
        if let Some(rev) = src.rev.as_ref() {
            args.extend_from_slice(&["--rev", rev.as_ref()]);
        }
        Ok(Self { name, cfg, args })
    }

    #[instrument(skip_all)]
    pub fn fetch(&self, path: impl AsRef<Path>) -> Result<()> {
        let status = std::process::Command::new(&self.cfg.command)
            .args(&self.args)
            .current_dir(path)
            .status()
            .map_err(FetcherError::Spawn)?;
        if !status.success() {
            return Err(FetcherError::ExitStatus(status.code()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use pretty_assertions::assert_eq;
    use test_log::test;
    use url::Url;

    #[test]
    fn test_prepare_fetcher_git() {
        let cfg = Config::default();
        let src = builds::Source {
            name: "test-build".into(),
            kind: "git".into(),
            rev: Some("main".into()),
            url: Url::parse("https://example.com/").unwrap(),
        };
        let fetcher = Fetcher::for_source(&cfg.fetchers, &src).unwrap();
        assert_eq!(
            Fetcher {
                name: "git",
                cfg: &FetcherConfig {
                    command: "gorgon-build-fetch-git".into()
                },
                args: vec!["--url", "https://example.com/", "--rev", "main"]
            },
            fetcher
        );
    }

    #[test]
    fn test_prepare_fetcher_unconfigured() {
        let err = Fetcher::for_source(
            &Config::default().fetchers,
            &builds::Source {
                name: "test-build".into(),
                kind: "cvs".into(),
                rev: Some("main".into()),
                url: Url::parse("https://example.com/").unwrap(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, FetcherError::UnknownFetcher(name) if name == "cvs"));
    }

    #[test]
    fn test_fetch_echo() {
        let fetcher = Fetcher {
            name: "sh",
            cfg: &FetcherConfig {
                command: which::which("sh").expect("Couldn't find `sh`"),
            },
            args: vec!["-c", "echo 'hello' | tee test.txt"],
        };

        let tmpdir = tempfile::tempdir().expect("Couldn't create temporary directory");
        fetcher.fetch(tmpdir.path()).expect("Couldn't run fetcher");

        let contents = std::fs::read_to_string(tmpdir.path().join("test.txt"))
            .expect("Couldn't read expected output");
        assert_eq!(contents, "hello\n".to_string());
    }
}
