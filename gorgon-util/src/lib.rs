// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

#[cfg(feature = "json")]
pub mod json;
#[cfg(feature = "source_url")]
pub mod source_url;

pub trait WithAdvice<T, E> {
    fn with_advice(
        self,
        msg: &'static str,
        src: impl Into<miette::NamedSource<String>>,
    ) -> Result<T, AdviceWrapper>;
}
impl<T> WithAdvice<T, miette::Report> for Result<T, miette::Report> {
    fn with_advice(
        self,
        msg: &'static str,
        src: impl Into<miette::NamedSource<String>>,
    ) -> Result<T, AdviceWrapper> {
        self.map_err(|err| AdviceWrapper::new(err, Advice::new(msg, src)))
    }
}

#[derive(Debug)]
pub struct AdviceWrapper(miette::Report, [Advice; 1]);
impl AdviceWrapper {
    pub fn new(err: miette::Report, advice: Advice) -> Self {
        Self(err, [advice])
    }
}
impl std::fmt::Display for AdviceWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for AdviceWrapper {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}
impl miette::Diagnostic for AdviceWrapper {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.0.code()
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.0.severity()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.0.help()
    }

    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.0.url()
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.0.source_code()
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        self.0.labels()
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn miette::Diagnostic> + 'a>> {
        Some(Box::new(
            (self.1.iter().map(|v| v as _)).chain(self.0.related().into_iter().flatten()),
        ))
    }

    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        self.0.diagnostic_source()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Advice(&'static str, miette::NamedSource<String>);
impl Advice {
    pub fn new(msg: &'static str, src: impl Into<miette::NamedSource<String>>) -> Self {
        Self(msg, src.into())
    }
}
impl miette::Diagnostic for Advice {
    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::Advice)
    }
    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        Some(&self.1)
    }
    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        Some(Box::new(
            [miette::LabeledSpan::new(
                Some("".to_string()),
                0,
                self.1.inner().len(),
            )]
            .into_iter(),
        ))
    }
}
