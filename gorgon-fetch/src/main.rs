// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::borrow::Cow;

use clap::Parser;
use miette::{Context, IntoDiagnostic};
use url::Url;

/// Tool for fetching URLs.
///
/// The actual fetching is done by helpers, determined by the URL scheme:
///
/// gorgon-fetch     https://example.com -> gorgon-fetch-https https://example.com
/// gorgon-fetch git+https://example.com -> gorgon-fetch-git   https://example.com
///
/// Query parameters are turned into commandline arguments:
///
/// gorgon-fetch git+https://example.com?rev=73d6baaddf2a8972e4a24c1867a89874efee8a24
/// gorgon-fetch-git --rev=73d6baaddf2a8972e4a24c1867a89874efee8a24 https://example.com
///
/// See the documentation for each `gorgon-fetch-*` helper for more specifics.
#[derive(Debug, Clone, Parser)]
#[command(version, verbatim_doc_comment)]
pub struct Args {
    /// URL to fetch.
    url: Url,

    #[command(flatten)]
    fetcher: gorgon_build_helper::fetcher::FetcherArgs,

    #[command(flatten)]
    helpers: gorgon_util_cli::HelperArgs,
    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

fn split_fetcher_url(url: &Url) -> miette::Result<(&str, Cow<'_, Url>)> {
    match url {
        url if url.scheme().contains("+") => {
            // For some reason, `url.set_scheme` just... doesn't want to change schemes sometimes.
            // Let's just chop off the prefix and re-parse the URL, I guess.
            let (fetcher_name, rest) = url.as_str().split_once("+").unwrap();
            let new_url = Url::parse(rest)
                .into_diagnostic()
                .with_context(|| rest.to_string())?;
            Ok((fetcher_name, Cow::Owned(new_url)))
        }
        url => Ok((url.scheme(), Cow::Borrowed(url))),
    }
}

fn run(args: Args) -> miette::Result<()> {
    gorgon_util_cli::logger(args.common.level()).init();

    let (fetcher_name, mut url) = split_fetcher_url(&args.url).context("Couldn't parse URL")?;
    let mut helper = args
        .helpers
        .helper(format!("gorgon-fetch-{fetcher_name}"))?
        .with_arg_opt(args.common.level_flag())
        .with_args(args.fetcher.iter());

    if url.query().is_some() {
        helper = helper.with_args(
            url.as_ref()
                .query_pairs()
                .map(|(k, v)| format!("--{k}={v}")),
        );
        url.to_mut().set_query(None);
    }

    helper
        .with_arg(url.as_ref().as_str())
        .run()
        .map_err(|err| err.into())
}

fn main() {
    gorgon_util_cli::main(run);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_split_fetcher_url_git_https() {
        assert_eq!(
            (
                "git",
                Cow::Owned(Url::parse("https://example.com/").unwrap())
            ),
            split_fetcher_url(&Url::parse("git+https://example.com/").unwrap()).unwrap()
        );
    }

    #[test]
    fn test_split_fetcher_url_https() {
        assert_eq!(
            (
                "https",
                Cow::Borrowed(&Url::parse("https://example.com/").unwrap())
            ),
            split_fetcher_url(&Url::parse("https://example.com/").unwrap()).unwrap()
        );
    }
}
