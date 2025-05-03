// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{
    fmt::Debug,
    io::{IsTerminal, Write},
};

use chrono::prelude::*;
use comfy_table::{Cell, ColumnConstraint, Table};
use gorgond_client::{models, Uuid};
use miette::{IntoDiagnostic, NamedSource, Result};
use serde::Serialize;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Format {
    Default,
    Json,
}

#[derive(Debug, Clone, clap::Parser)]
#[command(next_help_heading = "Output")]
pub struct OutputArgs {
    /// Output format.
    ///
    /// By default, output will attempt to be more human-friendly if stdout is a TTY.
    #[arg(long, short)]
    format: Option<Format>,

    /// Colour output, or explicitly disable it with --color=false.
    ///
    /// By default, output will be coloured if stdout is a TTY.
    #[arg(long, short, default_missing_value = "true", require_equals = true, num_args=0..=1)]
    color: Option<bool>,
}
impl OutputArgs {
    pub fn stdout(&self) -> Format {
        self.format
            .unwrap_or_else(|| match std::io::stdout().is_terminal() {
                true => Format::Default,
                false => Format::Json,
            })
    }
    pub fn stdout_color(&self) -> bool {
        self.color
            .unwrap_or_else(|| std::io::stdout().is_terminal())
    }

    pub fn with<V: Debug + Serialize, F: FnOnce(&V)>(&self, f: F, v: &V) -> Result<()> {
        match self.stdout() {
            Format::Default => {
                f(v);
                Ok(())
            }
            Format::Json => self.dump(v),
        }
    }

    pub fn dump<V: Debug + Serialize>(&self, v: &V) -> Result<()> {
        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("Serializing output failed")]
        struct DumpJsonError(
            #[source] serde_json::Error,
            #[source_code] NamedSource<String>,
        );

        let mode = match self.stdout_color() {
            true => colored_json::ColorMode::On,
            false => colored_json::ColorMode::Off,
        };
        let mut stdout = std::io::stdout();
        colored_json::write_colored_json_with_mode(&v, &mut stdout, mode)
            .map_err(|err| DumpJsonError(err, NamedSource::new("", format!("{v:#?}"))))?;
        stdout.write_all(b"\n").into_diagnostic()
    }
}

macro_rules! to_cell {
    ($t:ty) => {
        impl ToCell for $t {
            fn cell(&self) -> Cell {
                Cell::new(self)
            }
        }
    };
}

trait ToCell {
    fn cell(&self) -> Cell;
}
impl<T: ToCell> ToCell for Option<T> {
    fn cell(&self) -> Cell {
        match self.as_ref() {
            Some(v) => v.cell(),
            None => Cell::new("-").add_attribute(comfy_table::Attribute::Dim),
        }
    }
}
impl ToCell for Uuid {
    fn cell(&self) -> Cell {
        Cell::new(self.to_string())
            .set_delimiter('-')
            .add_attribute(comfy_table::Attribute::Dim)
    }
}
impl ToCell for DateTime<Utc> {
    fn cell(&self) -> Cell {
        Cell::new(self.format("%Y-%m-%d %H:%M:%S")).set_delimiter(' ')
    }
}
to_cell!(std::borrow::Cow<'_, str>);
to_cell!(models::BuildResult);
to_cell!(models::BuildTaskLevel);
to_cell!(models::BuildTaskKind);

fn cell<V: ToCell>(v: &V) -> Cell {
    ToCell::cell(v)
}
fn cell_or<B: ToCell, A, F: FnOnce(&A) -> Cell>(b: &B, o: &Option<A>, f: F) -> Cell {
    match o {
        Some(a) => f(a),
        None => b.cell(),
    }
}

fn table() -> Table {
    let mut table = comfy_table::Table::new();
    table
        .load_preset(comfy_table::presets::UTF8_FULL)
        .apply_modifier(comfy_table::modifiers::UTF8_ROUND_CORNERS)
        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic);
    table
}

pub fn build_table<'a>(
    builds: impl IntoIterator<
        Item = (
            &'a models::Build<'a>,
            Option<&'a models::Project<'a>>,
            Option<&'a models::Worker<'a>>,
        ),
    >,
) {
    let mut table = table();
    table
        .set_header([
            "ID", "Project", "Kind", "Source", "Worker", "Result", "Started", "Ended",
        ])
        .set_constraints([ColumnConstraint::ContentWidth])
        .add_rows(builds.into_iter().map(|(b, p, w)| {
            [
                cell(&b.id),
                cell_or(&b.project_id, &p, |p| p.spec.slug.cell()),
                cell(&b.spec.kind),
                cell(&b.spec.source),
                cell_or(&b.worker_id, &w, |w| w.spec.slug.cell()),
                cell(&b.result),
                cell(&b.started_at),
                cell(&b.ended_at),
            ]
        }));
    println!("{table}");
}
