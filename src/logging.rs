//! Structured logging with `tracing`.
//!
//! Events go to three places:
//! * a daily rotating log file (`%LOCALAPPDATA%\AeternaVault\logs`, 14 days kept),
//! * an in-memory ring buffer shown in the GUI's "Activity" view,
//! * stderr when running as a command-line tool (warnings and errors only).
//!
//! Libraries (wgpu, egui, winit …) log a lot of technical detail. Only their
//! warnings and errors reach the log file, and the Activity view shows
//! AeternaVault's own messages only.
//!
//! Set `AETERNAVAULT_LOG=debug` for more detail.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::{LevelFilter, filter_fn};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, fmt};

use crate::paths::AppPaths;

const BUFFER_LINES: usize = 2000;

/// Events of this crate have targets like `AeternaVault::gui::actions`.
const OWN_TARGET: &str = env!("CARGO_CRATE_NAME");

pub fn is_own_event(metadata: &Metadata<'_>) -> bool {
    let target = metadata.target();
    target == OWN_TARGET
        || target
            .strip_prefix(OWN_TARGET)
            .is_some_and(|rest| rest.starts_with("::"))
}

/// Own events at the chosen level; other crates only with warnings and errors.
fn file_worthy(metadata: &Metadata<'_>) -> bool {
    is_own_event(metadata) || *metadata.level() <= Level::WARN
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub time: DateTime<Local>,
    pub level: Level,
    pub message: String,
}

#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<VecDeque<LogLine>>>);

impl LogBuffer {
    fn push(&self, line: LogLine) {
        if let Ok(mut lines) = self.0.lock() {
            if lines.len() >= BUFFER_LINES {
                lines.pop_front();
            }
            lines.push_back(line);
        }
    }

    pub fn lines(&self) -> Vec<LogLine> {
        self.0
            .lock()
            .map(|l| l.iter().cloned().collect())
            .unwrap_or_default()
    }
}

pub struct Logging {
    pub buffer: LogBuffer,
    /// Flushes the log file when dropped at the end of `main`.
    _file_guard: Option<WorkerGuard>,
}

pub fn init(paths: &AppPaths, console: bool) -> Logging {
    let level = match std::env::var("AETERNAVAULT_LOG").as_deref() {
        Ok("trace") => LevelFilter::TRACE,
        Ok("debug") => LevelFilter::DEBUG,
        Ok("warn") => LevelFilter::WARN,
        Ok("info") => LevelFilter::INFO,
        // Otherwise as chosen in the settings (Settings → Logging).
        _ => match crate::config::log_level(paths) {
            crate::config::LogLevel::Errors => LevelFilter::WARN,
            crate::config::LogLevel::Normal => LevelFilter::INFO,
            crate::config::LogLevel::Detailed => LevelFilter::DEBUG,
        },
    };

    let buffer = LogBuffer::default();
    // The appender cleans up old files on start and complains if the folder is missing.
    let _ = std::fs::create_dir_all(&paths.log_dir);

    let (file_layer, guard) = match RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("aeterna-vault")
        .filename_suffix("log")
        .max_log_files(14)
        .build(&paths.log_dir)
    {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(writer)
                .with_filter(filter_fn(file_worthy));
            (Some(layer), Some(guard))
        }
        Err(err) => {
            eprintln!("log file could not be opened: {err}");
            (None, None)
        }
    };

    let console_layer = (console || cfg!(debug_assertions)).then(|| {
        fmt::layer()
            .with_target(false)
            .with_writer(std::io::stderr)
            .with_filter(if console { LevelFilter::WARN } else { level })
            .with_filter(filter_fn(file_worthy))
    });

    let _ = tracing_subscriber::registry()
        .with(level)
        .with(file_layer)
        .with(console_layer)
        .with(
            GuiLayer {
                buffer: buffer.clone(),
            }
            .with_filter(filter_fn(is_own_event)),
        )
        .try_init();

    Logging {
        buffer,
        _file_guard: guard,
    }
}

struct GuiLayer {
    buffer: LogBuffer,
}

impl<S: Subscriber> Layer<S> for GuiLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let mut message = visitor.message;
        if !visitor.fields.is_empty() {
            message.push_str(&visitor.fields);
        }
        self.buffer.push(LogLine {
            time: Local::now(),
            level: *event.metadata().level(),
            message,
        });
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            let _ = write!(self.fields, "  {}={}", field.name(), value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            let _ = write!(self.fields, "  {}={:?}", field.name(), value);
        }
    }
}
