use url::{Position, Url};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error(transparent)]
    BadUrl(#[from] url::ParseError),

    #[error("Unknown fetcher: '{0}'")]
    #[diagnostic(
        help = "Fetchers are specified by the URL scheme, eg. 'git://' or 'git+http://'.\nSupported fetchers: 'git'."
    )]
    UnknownFetcher(String),

    #[error("Expected fetcher '{0}', found '{1}'")]
    ExpectedFetcher(&'static str, String),

    #[error("Unknown query parameter: '{0}'")]
    UnknownQueryParameter(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceUrl {
    Git(GitUrl),
}
impl SourceUrl {
    pub fn parse(s: &str) -> Result<Self> {
        Self::parse_url(&Url::parse(s)?)
    }
    pub fn parse_url(url: &Url) -> Result<Self> {
        match url_fetcher_base(url).0 {
            "git" => Ok(Self::Git(GitUrl::parse_url(url)?)),
            f => Err(Error::UnknownFetcher(f.into())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitUrl {
    pub base_url: Url,
    pub rev: Option<String>,
    pub ref_: Option<String>,
}
impl GitUrl {
    pub fn parse(s: &str) -> Result<Self> {
        Self::parse_url(&Url::parse(s)?)
    }
    pub fn parse_url(url: &Url) -> Result<Self> {
        let (fetcher, base_url) = url_fetcher_base(url);
        expect_fetcher("git", fetcher)?;

        let (mut rev, mut ref_) = (None, None);
        for (k, v) in url.query_pairs() {
            match &*k {
                "rev" => rev = Some(v.into()),
                "ref" => ref_ = Some(v.into()),
                k => return Err(Error::UnknownQueryParameter(k.into())),
            }
        }
        Ok(Self {
            base_url: Url::parse(base_url)?,
            rev,
            ref_,
        })
    }
}

fn expect_fetcher(expected: &'static str, actual: &str) -> Result<()> {
    if actual != expected {
        Err(Error::ExpectedFetcher(expected, actual.into()))
    } else {
        Ok(())
    }
}

fn url_fetcher_base(url: &Url) -> (&str, &str) {
    let scheme = url.scheme();
    let base_url = &url[..Position::AfterPath];
    match scheme.split_once("+") {
        Some((fetcher, _)) => (fetcher, &base_url[fetcher.len() + 1..]),
        None => (scheme, base_url),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_git_https() {
        assert_eq!(
            SourceUrl::Git(GitUrl {
                base_url: Url::parse("https://github.com/NixOS/nixpkgs").unwrap(),
                rev: Some("63e24fbc205fc7484ac7c90212a94853be116de4".into()),
                ref_: None,
            }),
            SourceUrl::parse(concat!(
                "git+https://github.com/NixOS/nixpkgs",
                "?rev=63e24fbc205fc7484ac7c90212a94853be116de4"
            ))
            .unwrap(),
        );
    }
    #[test]
    fn test_parse_git() {
        assert_eq!(
            SourceUrl::Git(GitUrl {
                base_url: Url::parse("git://github.com/NixOS/nixpkgs").unwrap(),
                rev: Some("63e24fbc205fc7484ac7c90212a94853be116de4".into()),
                ref_: None
            }),
            SourceUrl::parse(concat!(
                "git://github.com/NixOS/nixpkgs",
                "?rev=63e24fbc205fc7484ac7c90212a94853be116de4"
            ))
            .unwrap(),
        );
    }

    #[test]
    fn test_url_fetcher_base_git() {
        assert_eq!(
            ("git", "git://github.com/NixOS/nixpkgs"),
            url_fetcher_base(&Url::parse("git://github.com/NixOS/nixpkgs").unwrap())
        );
    }
    #[test]
    fn test_url_fetcher_base_git_https() {
        assert_eq!(
            ("git", "https://github.com/NixOS/nixpkgs"),
            url_fetcher_base(&Url::parse("git+https://github.com/NixOS/nixpkgs").unwrap())
        );
    }
}
