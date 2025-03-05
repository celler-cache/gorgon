// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use clap::Parser;
use gorgon_build_helper::Uuid;
use log::debug;
use miette::{miette, Context, IntoDiagnostic, Result};
use nix_daemon::{
    nix::internal_json::{
        Action, ActionMessage, ActionResult, ActionResultData, ActionStart, ActionStop,
        BuildLogLine, FileLinked, Progress, SetExpected, SetPhase,
    },
    StderrActivityType, Verbosity,
};
use owo_colors::{style, OwoColorize, Style};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{ChildStderr, Command, Stdio},
    sync::Arc,
};

const BLANK: &str = "";

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Nix command to run.
    #[arg(long, short = 'C', default_value = "nix-build")]
    nix_build: PathBuf,

    /// Path to the source tree to build.
    source_path: PathBuf,

    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

#[derive(Debug, Default)]
pub struct BuildState {
    pub act_by_id: BTreeMap<u64, ActivityState>,
    pub act_ids_by_parent: BTreeMap<u64, BTreeSet<u64>>,
    pub stats: BuildStats,
}

impl BuildState {
    pub fn start(&mut self, act: ActivityState) {
        self.act_ids_by_parent
            .entry(act.parent)
            .or_default()
            .insert(act.id);
        self.act_by_id.insert(act.id, act);
    }

    pub fn stop(&mut self, id: u64) {
        if let Some(act) = self.act_by_id.remove(&id) {
            if let Some(set) = self.act_ids_by_parent.get_mut(&act.parent) {
                set.remove(&id);
            }
        }
    }

    fn display_by_parent(
        &self,
        f: &mut std::fmt::Formatter<'_>,
        parent: u64,
        indent: usize,
    ) -> std::fmt::Result {
        if let Some(ids) = self.act_ids_by_parent.get(&parent) {
            let width = f.width().unwrap_or(80) - indent;
            for act in ids.iter().filter_map(|id| self.act_by_id.get(id)) {
                writeln!(f, "{BLANK:bw$}{act:width$}", bw = indent * 3)?;
                self.display_by_parent(f, act.id, indent + 1)?;
            }
        }
        Ok(())
    }
}

impl Display for BuildState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.display_by_parent(f, 0, 0)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct ActivityState {
    // Start
    pub id: u64,
    pub level: Verbosity,
    pub parent: u64,
    pub text: String,
    pub type_: StderrActivityType,

    // SetPhase
    pub phase: Option<String>,

    // Progress
    pub progress: Option<ActivityProgress>,
}

impl Default for ActivityState {
    fn default() -> Self {
        Self {
            id: 0,
            level: Verbosity::Vomit,
            parent: 0,
            text: String::default(),
            type_: StderrActivityType::Unknown,
            phase: None,
            progress: None,
        }
    }
}

impl Display for ActivityState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let icon = match self.type_ {
            StderrActivityType::Unknown => "  ",
            StderrActivityType::CopyPath | StderrActivityType::CopyPaths => "📂",
            StderrActivityType::FileTransfer => "📡",
            StderrActivityType::Realise => "🪄",
            StderrActivityType::Build | StderrActivityType::Builds => "🚧",
            StderrActivityType::OptimiseStore => "🚂",
            StderrActivityType::VerifyPaths => "🔬",
            StderrActivityType::Substitute => "🏗️",
            StderrActivityType::QueryPathInfo => "🔍",
            StderrActivityType::PostBuildHook => "🔜",
            StderrActivityType::BuildWaiting => "⏸️",
        };
        write!(f, "{icon}")?;
        if let Some(phase) = &self.phase {
            write!(f, " {}", phase.dimmed())?;
        }

        if let Some(progress) = &self.progress {
            write!(f, " {progress:20}")?;
        }

        if !self.text.is_empty() {
            write!(f, " {}", self.text)?;
        } else {
            write!(f, " {}", self.type_.as_ref().dimmed())?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct ActivityProgress {
    pub done: u64,
    pub expected: u64,
    pub running: u64,
    pub failed: u64,
}

impl Display for ActivityProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let width = f.width().unwrap_or(20) as u64;
        let total = std::cmp::max(self.expected, self.done + self.running + self.failed);
        let failed_style = style().red().dimmed();
        let failed_w = match Self::write_seg(f, width, self.failed, total, &failed_style, "▓")? {
            0 if self.failed > 0 => 1,
            v => v,
        };
        let done_style = style().green().dimmed();
        let done_w = Self::write_seg(f, width, self.done, total, &done_style, "█")?;
        let running_style = style().blue().dimmed();
        let running_w = Self::write_seg(f, width, self.running, total, &running_style, "░")?;
        let rest_w = width - failed_w - done_w - running_w;
        write!(f, "{BLANK:rest_w$}", rest_w = rest_w as usize)
    }
}

impl ActivityProgress {
    fn write_seg(
        f: &mut std::fmt::Formatter<'_>,
        bar_width: u64,
        value: u64,
        total: u64,
        style: &Style,
        fill: &str,
    ) -> Result<u64, std::fmt::Error> {
        let width = (bar_width as f64 * (value as f64 / total as f64)) as u64;
        if width > 0 {
            style.fmt_prefix(f)?;
            (0..width).try_for_each(|_| write!(f, "{fill}"))?;
            style.fmt_suffix(f)?;
        }
        Ok(width)
    }
}

#[derive(Debug, Default)]
pub struct ActivityTypeStats {
    pub done: u64,
    pub expected: u64,
    pub failed: u64,
}

#[derive(Debug, Default)]
pub struct BuildStats {
    pub files_linked: u64,
    pub bytes_linked: u64,
    pub untrusted_paths: u64,
    pub corrupted_paths: u64,
    pub activities_by_type: BTreeMap<StderrActivityType, ActivityTypeStats>,
}

fn handle_stderr(stderr: ChildStderr, view: Arc<nutmeg::View<BuildState>>) -> Result<()> {
    let mut reader = BufReader::new(stderr);
    let mut line = String::with_capacity(1024);
    loop {
        line.clear();
        if reader
            .read_line(&mut line)
            .into_diagnostic()
            .context("Couldn't read line from stderr")?
            == 0
        {
            debug!("End of child stderr");
            break;
        }
        if let Some(json) = line.strip_prefix("@nix ") {
            let action: Action = serde_json::from_str(json)
                .into_diagnostic()
                .context("Couldn't parse nix log line")?;
            match action {
                Action::Start(ActionStart {
                    id,
                    level,
                    parent,
                    text,
                    type_,
                }) => {
                    view.update(|state| {
                        state.start(ActivityState {
                            id,
                            level,
                            parent,
                            text,
                            type_,
                            phase: None,
                            progress: None,
                        })
                    });
                }
                Action::Stop(ActionStop { id }) => view.update(|state| state.stop(id)),
                Action::Message(ActionMessage {
                    file,
                    column: _,
                    line,
                    level,
                    msg,
                }) => {
                    let meta = log::Metadata::builder()
                        .target("nix-build::stderr")
                        .level(match level {
                            Verbosity::Error => log::Level::Error,
                            Verbosity::Warn => log::Level::Warn,
                            Verbosity::Notice | Verbosity::Info => log::Level::Info,
                            _ => log::Level::Trace,
                        })
                        .build();
                    let logger = log::logger();
                    if logger.enabled(&meta) {
                        logger.log(
                            &log::Record::builder()
                                .metadata(meta)
                                .args(format_args!("{msg}"))
                                .file(file.as_deref())
                                .line(
                                    line.map(|l| l.try_into().expect("impossibly big line number")),
                                )
                                .build(),
                        );
                    }
                }
                Action::Result(ActionResult { id, data }) => match data {
                    ActionResultData::FileLinked(FileLinked { bytes_linked }) => {
                        view.update(|state| {
                            state.stats.files_linked += 1;
                            state.stats.bytes_linked += bytes_linked;
                        });
                    }
                    ActionResultData::BuildLogLine(BuildLogLine { line })
                    | ActionResultData::PostBuildLogLine(BuildLogLine { line }) => {
                        view.message(line.dimmed().to_string());
                    }
                    ActionResultData::UntrustedPath => {
                        view.update(|state| state.stats.untrusted_paths += 1)
                    }
                    ActionResultData::CorruptedPath => {
                        view.update(|state| state.stats.corrupted_paths += 1)
                    }
                    ActionResultData::SetPhase(SetPhase { phase }) => view.update(|state| {
                        if let Some(act) = state.act_by_id.get_mut(&id) {
                            act.phase = Some(phase);
                        }
                    }),
                    ActionResultData::Progress(Progress {
                        done,
                        expected,
                        running,
                        failed,
                    }) => view.update(|state| {
                        if let Some(act) = state.act_by_id.get_mut(&id) {
                            act.progress = Some(ActivityProgress {
                                done,
                                expected,
                                running,
                                failed,
                            });
                        }
                    }),
                    ActionResultData::SetExpected(SetExpected {
                        activity_type,
                        expected,
                    }) => view.update(|state| {
                        if let Some(act) = state.act_by_id.get_mut(&id) {
                            if let Some(typestats) =
                                state.stats.activities_by_type.get_mut(&activity_type)
                            {
                                typestats.expected -=
                                    act.progress.as_ref().map(|p| p.expected).unwrap_or(0);
                                typestats.expected += expected;
                            }
                            act.progress.get_or_insert_default().expected = expected;
                        }
                    }),
                },
            };
        }
    }
    Ok(())
}

fn run(args: Args) -> Result<()> {
    let view = Arc::new(nutmeg::View::new(
        BuildState::default(),
        nutmeg::Options::default(),
    ));

    // This will be replaced with calls to the `nix-daemon` crate in the future.
    // Unfortunately, the client does *a lot* of... not even lifting, random errands?
    let mut cmd = Command::new(args.nix_build);
    cmd.arg("--no-out-link")
        .args(["--log-format", "internal-json"])
        .arg(args.source_path)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped());
    debug!(cmd:?; "Spawning...");

    let mut child = cmd
        .spawn()
        .into_diagnostic()
        .context("Couldn't spawn `nix-build`")?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| miette!("Couldn't get child stderr!?"))?;
    let stderr_join = std::thread::spawn(move || handle_stderr(stderr, view));

    let status = child
        .wait()
        .into_diagnostic()
        .context("Couldn't wait for `nix-build`")?;
    if !status.success() {
        match status.code() {
            Some(code) => miette!("`nix-build` failed: {code}"),
            None => miette!("`nix-build` failed"),
        };
    }
    stderr_join.join().expect("stderr thread panicked")
}

fn main() {
    let args = Args::parse();
    gorgon_util_cli::logger(args.common.log_format, args.common.level()).init();
    gorgon_util_cli::main(args.common.output_format, || run(args));
}

/// Casts a Nix Activity ID into a UUIDv8.
///
/// Nix (as of Lix 2.92) generates Activity IDs using:
///
/// ```cpp
/// std::atomic<uint64_t> nextId{0};
/// Activity::Activity(...): id(nextId++ + (((uint64_t) getpid()) << 32)) { ... }
/// ```
///
/// In other words, it's effectively a tuple of (pid: u32, id: u32) mashed together.
pub fn activity_uuid(id: u64) -> Uuid {
    let id_bytes: [u8; 8] = id.to_be_bytes();
    let mut data = [0u8; 16];
    data[0..4].copy_from_slice(&id_bytes[0..4]);
    data[12..16].copy_from_slice(&id_bytes[4..8]);
    Uuid::new_v8(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fmt::Write;

    #[test]
    fn test_activity_uuid_1122_3344_5566_7788() {
        assert_eq!(
            &activity_uuid(0x1122_3344_5566_7788).to_string(),
            "11223344-0000-8000-8000-000055667788"
        );
    }

    #[test]
    fn test_activity_uuid_17368302284177417() {
        assert_eq!(
            &activity_uuid(17368302284177417).to_string(),
            "003db461-0000-8000-8000-000000000009"
        );
    }

    #[test]
    fn test_progress_empty() {
        let mut s = String::new();
        write!(&mut s, "{}", ActivityProgress::default()).unwrap();
        assert_eq!(&s, "                    ");
    }

    #[test]
    fn test_progress_full() {
        let mut s = String::new();
        write!(
            &mut s,
            "{}",
            ActivityProgress {
                done: 10,
                expected: 10,
                running: 0,
                failed: 0
            }
        )
        .unwrap();
        assert_eq!(&strip_ansi_escapes::strip_str(s), "████████████████████");
    }

    #[test]
    fn test_progress_half() {
        let mut s = String::new();
        write!(
            &mut s,
            "{}",
            ActivityProgress {
                done: 5,
                expected: 10,
                running: 0,
                failed: 0
            }
        )
        .unwrap();
        assert_eq!(&strip_ansi_escapes::strip_str(s), "██████████          ");
    }
}
