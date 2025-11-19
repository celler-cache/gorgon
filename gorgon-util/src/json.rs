// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use miette::{Diagnostic, LabeledSpan, NamedSource, SourceOffset, SourceSpan};

use crate::{Advice, AdviceWrapper};

pub type Result<T, E = JsonError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
#[error("JSON Error")]
pub struct JsonError(NamedSource<String>, #[source] serde_json::Error);
impl Diagnostic for JsonError {
    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        Some(&self.0)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        Some(Box::new(
            [
                LabeledSpan::new_primary_with_span(
                    Some(self.1.to_string()),
                    SourceSpan::new(
                        SourceOffset::from_location(self.0.inner(), self.1.line(), self.1.column()),
                        0,
                    ),
                ),
                LabeledSpan::new(None, 0, self.0.inner().len()),
            ]
            .into_iter(),
        ))
    }
}

pub trait WithJsonAdvice<T, E> {
    fn with_json_advice<V: serde::Serialize>(
        self,
        msg: &'static str,
        v: &V,
    ) -> Result<T, AdviceWrapper>;
}
impl<T> WithJsonAdvice<T, miette::Report> for Result<T, miette::Report> {
    fn with_json_advice<V: serde::Serialize>(
        self,
        msg: &'static str,
        v: &V,
    ) -> Result<T, AdviceWrapper> {
        self.map_err(|err| {
            AdviceWrapper::new(
                err,
                Advice::new(
                    msg,
                    NamedSource::new(
                        std::any::type_name_of_val(v),
                        serde_json::to_string_pretty(v).unwrap_or_else(|err| err.to_string()),
                    )
                    .with_language("JSON"),
                ),
            )
        })
    }
}

pub fn parse_json<'a, T: serde::Deserialize<'a>>(name: &str, src: &'a str) -> Result<T> {
    serde_json::from_str(src).map_err(|err| {
        JsonError(
            NamedSource::new(name, src.to_string()).with_language("JSON"),
            err,
        )
    })
}
