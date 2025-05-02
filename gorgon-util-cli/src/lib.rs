// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{
    borrow::Cow,
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use clap::Parser;
use colored_json::write_colored_json_with_mode;
use gorgon_build_helper::{
    helper::{Helper, HelperCaller, HelperContext, HelperFinder},
    Event, EventHandler, SerializedError,
};
use gorgond_client::{
    events::{
        BuildEvent, BuildEventKind, BuildTaskEvent, BuildTaskEventKind, BuildTaskUpdate, Log,
    },
    models::BuildTask,
    Uuid,
};
use log::{error, info, log, log_enabled, Level};
use miette::{miette, Context, IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use parking_lot::RwLock;
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};

#[derive(Clone, Copy)]
struct EventLoggerTask {
    level: log::Level,
    started: Instant,
}

#[derive(Default)]
pub struct EventLogger {
    tasks: RwLock<BTreeMap<Uuid, EventLoggerTask>>,
}
impl EventHandler for EventLogger {
    fn handle(&self, target: &str, event: Event<'_>) {
        let log_log = |lvl: Level, log: Log| {
            log::logger().log(
                &log::Record::builder()
                    .target(target)
                    .level(lvl)
                    .file(log.file.as_deref())
                    .line(log.line.map(|v| v as _))
                    .key_values(&("kind", log.kind.as_ref()))
                    .args(format_args!("{}", log.text))
                    .build(),
            )
        };

        match event {
            Event::Error(err) => {
                let e = format!("{:?}", miette::Report::new(err));
                error!(__log__=e; "Error")
            }
            Event::Log(log) => log.log(target),
            Event::Build(BuildEvent { event, .. }) => match event {
                BuildEventKind::Started => info!(target: target, "Build: Started"),
                BuildEventKind::Stopped => info!(target: target, "Build: Stopped"),
                BuildEventKind::Log(log) => log_log(Level::Info, log),
                BuildEventKind::Task(BuildTaskEvent { task_id, event }) => {
                    let task: EventLoggerTask =
                        match event {
                            BuildTaskEventKind::Started(BuildTask { level, .. }) => {
                                let task = EventLoggerTask {
                                    level: level.into(),
                                    started: Instant::now(),
                                };
                                self.tasks.write().insert(task_id, task);
                                task
                            }
                            _ => self.tasks.read().get(&task_id).copied().unwrap_or(
                                EventLoggerTask {
                                    level: Level::Trace,
                                    started: Instant::now(),
                                },
                            ),
                        };

                    let lvl = match event {
                        BuildTaskEventKind::Started(_) | BuildTaskEventKind::Stopped => task.level,
                        BuildTaskEventKind::Log(Log { level, .. }) => level.into(),
                        BuildTaskEventKind::Updated(ref update) => match update {
                            BuildTaskUpdate::UntrustedPath | BuildTaskUpdate::CorruptedPath => {
                                Level::Warn
                            }
                            BuildTaskUpdate::FileLinked { .. }
                            | BuildTaskUpdate::SetPhase { .. } => Level::Debug,
                            BuildTaskUpdate::Progress { .. } => Level::Trace,
                        },
                    };
                    if log_enabled!(target: target, lvl) {
                        match event {
                            BuildTaskEventKind::Started(BuildTask {
                                level, kind, name, ..
                            }) => {
                                let level: &'static str = level.into();
                                let kind: &'static str = kind.map(|v| v.into()).unwrap_or("???");
                                log!(target: target, lvl, name, level, kind; "+ Started");
                            }
                            BuildTaskEventKind::Stopped => {
                                let t = humantime::Duration::from(Instant::now() - task.started);
                                log!(target: target, lvl, t:%; "- Stopped");
                            }
                            BuildTaskEventKind::Updated(update) => match update {
                                BuildTaskUpdate::FileLinked { bytes } => {
                                    log!(target: target, lvl, bytes; "  -> File Linked")
                                }
                                BuildTaskUpdate::UntrustedPath => {
                                    log!(target: target, lvl, "  -> Untrusted Path")
                                }
                                BuildTaskUpdate::CorruptedPath => {
                                    log!(target: target, lvl, "  -> Corrupted Path")
                                }
                                BuildTaskUpdate::SetPhase { phase } => {
                                    log!(target: target, lvl, phase; "  -> Phase Change")
                                }
                                BuildTaskUpdate::Progress {
                                    done,
                                    expected,
                                    running,
                                    failed,
                                } => {
                                    log!(target: target, lvl, done, expected, running, failed; "  -> Progress")
                                }
                            },
                            BuildTaskEventKind::Log(log) => log_log(lvl, log),
                        };
                    }
                }
            },
        }
    }
}

#[derive(Default)]
struct EventSerializer;
impl EventHandler for EventSerializer {
    fn handle(&self, _: &str, event: Event) {
        let mut stderr = std::io::stderr().lock();
        serde_json::to_writer(&mut stderr, &event).expect("Couldn't serialize event!?");
        let _ = writeln!(&mut stderr);
    }
}

#[derive(
    Debug,
    Display,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    EnumString,
    AsRefStr,
    IntoStaticStr,
    clap::ValueEnum,
)]
#[strum(serialize_all = "kebab-case")]
pub enum OutputFormat {
    #[default]
    Default,
    Json,
}

#[derive(Debug, Clone, Parser)]
#[group(id = "common")]
#[command(next_help_heading = "Logging")]
pub struct Args {
    /// Increase log level.
    #[arg(long, short, env = "GORGON_VERBOSE", action=clap::ArgAction::Count)]
    pub verbose: u8,

    /// Decrease log level.
    #[arg(long, short, env = "GORGON_QUIET", action=clap::ArgAction::Count)]
    pub quiet: u8,

    /// Output format for logs and events.
    #[arg(long, env = "GORGON_LOG_FORMAT", default_value = "default")]
    pub log_format: OutputFormat,

    /// Output format for output and errors.
    #[arg(long, env = "GORGON_OUTPUT_FORMAT", default_value = "default")]
    pub output_format: OutputFormat,
}

impl Args {
    pub fn level(&self) -> log::LevelFilter {
        match self.verbose as i8 - self.quiet as i8 {
            ..=-2 => log::LevelFilter::Error,
            -1 => log::LevelFilter::Warn,
            0 => log::LevelFilter::Info,
            1 => log::LevelFilter::Debug,
            2.. => log::LevelFilter::Trace,
        }
    }
}

impl HelperContext for &Args {
    fn apply(&self, mut helper: Helper) -> Helper {
        if self.verbose > 0 {
            helper = helper.with_env("GORGON_VERBOSE", self.verbose.to_string());
        }
        if self.quiet > 0 {
            helper = helper.with_env("GORGON_QUIET", self.quiet.to_string());
        }
        helper
            .with_env("GORGON_LOG_FORMAT", self.log_format.as_ref())
            .with_env("GORGON_OUTPUT_FORMAT", self.output_format.as_ref())
    }
}

#[derive(Debug, Clone, Parser)]
#[group(id = "helpers")]
#[command(next_help_heading = "Helpers")]
pub struct HelperArgs {
    #[cfg(not(windows))]
    /// Path(s) searched for helpers. Default: $PATH
    #[arg(long, env = "GORGON_HELPER_PATH", value_delimiter = ':')]
    helper_path: Vec<PathBuf>,

    #[cfg(windows)]
    /// Path(s) searched for helpers. Default: $PATH
    #[arg(long, env = "GORGON_HELPER_PATH", value_delimiter = ';')]
    helper_path: Vec<PathBuf>,

    /// Run helpers from source. Requires `cargo`.
    #[arg(long, env = "GORGON_HELPER_DEV")]
    helper_dev: bool,

    /// With --helper-dev, build from this repository.
    #[arg(long, env = "GORGON_HELPER_DEV_REPO")]
    helper_dev_repo: Option<PathBuf>,
}

impl HelperContext for HelperArgs {
    fn apply(&self, mut helper: Helper) -> Helper {
        if !self.helper_path.is_empty() {
            helper = helper.with_env(
                "GORGON_HELPER_PATH",
                std::env::join_paths(&self.helper_path).expect("Couldn't join GORGON_HELPER_PATH"),
            );
        }
        if self.helper_dev {
            helper = helper.with_env("GORGON_HELPER_DEV", "true");
        }
        if let Some(path) = self.helper_dev_repo.as_deref() {
            helper = helper.with_env("GORGON_HELPER_DEV_REPO", path);
        }
        helper
    }
}

impl HelperArgs {
    pub fn caller(&self) -> Result<impl HelperCaller + use<'_>> {
        let current_dir = std::env::current_dir()
            .into_diagnostic()
            .context("Couldn't get current working directory")?;

        let path = match &self.helper_path[..] {
            [] => Cow::Owned(
                std::env::var_os("PATH")
                    .ok_or_else(|| miette!("Neither $PATH nor --search-path is set"))?,
            ),
            [path] => Cow::Borrowed(path.as_ref()),
            paths => Cow::Owned(
                std::env::join_paths(paths)
                    .into_diagnostic()
                    .context("Couldn't join --search-path")?,
            ),
        };

        let mut caller = HelperFinder::new(current_dir.into(), path);
        if self.helper_dev {
            caller.with_dev(
                self.helper_dev_repo
                    .as_ref()
                    .map(|p| Cow::Borrowed(p.as_ref())),
            );
        }
        Ok(caller)
    }
}

#[must_use]
pub struct LoggerBuilder {
    format: OutputFormat,
    level: log::LevelFilter,
    target_levels: BTreeMap<Cow<'static, str>, log::LevelFilter>,
}

impl LoggerBuilder {
    pub fn with_format(mut self, format: OutputFormat) -> Self {
        self.format = format;
        self
    }

    pub fn with_target_level(
        mut self,
        target: impl Into<Cow<'static, str>>,
        level: log::LevelFilter,
    ) -> Self {
        self.target_levels.insert(target.into(), level);
        self
    }

    pub fn init(self) {
        let disp = self.target_levels.into_iter().fold(
            fern::Dispatch::new().level(self.level),
            |disp, (target, level)| disp.level_for(target, level),
        );
        let disp = match self.format {
            OutputFormat::Default => disp
                .format(move |out, message, record| {
                    let level = record.level();
                    let level_style = match level {
                        log::Level::Error => Style::new().red(),
                        log::Level::Warn => Style::new().yellow(),
                        log::Level::Info => Style::new(),
                        log::Level::Debug => Style::new().blue(),
                        log::Level::Trace => Style::new().dimmed(),
                    };
                    let text_style = match level {
                        log::Level::Info => Style::new(),
                        log::Level::Trace => Style::new().dimmed(),
                        _ => level_style,
                    };
                    let kv_style = match level {
                        log::Level::Debug | log::Level::Trace => level_style,
                        _ => text_style.dimmed(),
                    };

                    // fern doesn't have a good way to do key/value formatting atm:
                    // https://github.com/daboross/fern/issues/63
                    struct KVDisplay<'a>(&'a dyn log::kv::Source);
                    impl std::fmt::Display for KVDisplay<'_> {
                        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                            struct Visitor<'b, 'f>(&'b mut std::fmt::Formatter<'f>);
                            impl<'kv> log::kv::VisitSource<'kv> for Visitor<'_, '_> {
                                fn visit_pair(
                                    &mut self,
                                    key: log::kv::Key<'kv>,
                                    value: log::kv::Value<'kv>,
                                ) -> std::result::Result<(), log::kv::Error>
                                {
                                    match key.as_str() {
                                        "__log__" => Ok(()),
                                        _ => write!(self.0, " {key}={value}")
                                            .map_err(|_| log::kv::Error::msg("fmt::Error")),
                                    }
                                }
                            }
                            self.0
                                .visit(&mut Visitor(f))
                                .map_err(move |_| std::fmt::Error)
                        }
                    }

                    let log = record.key_values().get("__log__".into()).map(|log| {
                        textwrap::fill(
                            &format!("{log}"),
                            textwrap::Options::with_termwidth()
                                .initial_indent("  ")
                                .subsequent_indent("  "),
                        )
                    });

                    out.finish(format_args!(
                        "{:<5}  {}{}{}{}",
                        level.if_supports_color(Stream::Stderr, |v| v.style(level_style)),
                        format_args!("{target:<20}  {message}", target = record.target(),)
                            .if_supports_color(Stream::Stderr, |v| v.style(text_style)),
                        KVDisplay(record.key_values())
                            .if_supports_color(Stream::Stderr, |v| v.style(kv_style)),
                        log.as_ref().map(|_| "\n").unwrap_or(""),
                        log.as_deref()
                            .unwrap_or_default()
                            .if_supports_color(Stream::Stderr, |v| v.style(text_style)),
                    ))
                })
                .chain(std::io::stderr()),
            OutputFormat::Json => disp.chain(fern::Output::call(|rec| {
                let mut stderr = std::io::stderr().lock();
                serde_json::to_writer(
                    &mut stderr,
                    &gorgon_build_helper::Event::Log(
                        rec.try_into()
                            .expect("Couldn't turn log record into log event: {rec}"),
                    ),
                )
                .expect("Couldn't write JSON log line: {rec}");
                writeln!(stderr).expect("Couldn't write newline after JSON line: {rec}");
            })),
        };
        disp.apply().expect("Couldn't apply logger!?");
    }
}

pub fn logger(format: OutputFormat, level: log::LevelFilter) -> LoggerBuilder {
    LoggerBuilder {
        format,
        level,
        target_levels: BTreeMap::new(),
    }
}

pub fn main<E: Into<miette::Report>, F: FnOnce() -> Result<(), E>>(format: OutputFormat, f: F) {
    static MAIN_CALLED: AtomicBool = AtomicBool::new(false);
    assert!(
        !MAIN_CALLED.swap(true, Ordering::SeqCst),
        "Please don't call main() more than once."
    );

    match format {
        OutputFormat::Default => {
            miette::set_hook(Box::new(|_| {
                Box::new(
                    miette::MietteHandlerOpts::new()
                        .with_syntax_highlighting(
                            miette::highlighters::SyntectHighlighter::default(),
                        )
                        .context_lines(6)
                        .build(),
                )
            }))
            .expect("Couldn't install miette hook");

            // SAFETY: See MAIN_CALLED.
            unsafe {
                gorgon_build_helper::set_event_handler(Box::leak(Box::new(EventLogger::default())))
            };
        }
        OutputFormat::Json => {
            // SAFETY: See MAIN_CALLED.
            unsafe { gorgon_build_helper::set_event_handler(Box::leak(Box::new(EventSerializer))) };
        }
    }

    if let Err(err) = f().map_err(|err| err.into()) {
        match format {
            OutputFormat::Default => eprintln!("\n{err:?}"),
            OutputFormat::Json => {
                write_colored_json_with_mode(
                    &SerializedError::new(AsRef::<dyn miette::Diagnostic>::as_ref(&err)),
                    &mut std::io::stderr(),
                    colored_json::ColorMode::Auto(colored_json::Output::StdErr),
                )
                .expect("Couldn't serialize error");
                println!();
            }
        }
        std::process::exit(1);
    }
}
