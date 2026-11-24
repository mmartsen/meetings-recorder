use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

/// Number of daily log files kept on disk before the oldest is deleted.
const MAX_LOG_FILES: usize = 14;

/// Default filter when `RUST_LOG` is unset: our own crate at debug (detection decisions are
/// the main thing worth having on disk when diagnosing a missed meeting), everything else at
/// info so zbus/ksni chatter stays out.
const DEFAULT_FILTER: &str = "info,meeting_recorder=debug";

/// Where log files go: `$XDG_STATE_HOME/meeting-recorder/logs` (usually
/// `~/.local/state/meeting-recorder/logs`).
pub fn log_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("meeting-recorder")
        .join("logs")
}

/// Sets up two sinks:
/// - stderr (picked up by journald when run as a systemd --user service), filtered by
///   `RUST_LOG`, default `info`.
/// - a daily-rotated file in [`log_dir`], filtered by `MEETING_RECORDER_FILE_LOG`, default
///   [`DEFAULT_FILTER`], so a long-running test leaves a detailed trail even if stderr is
///   only at info.
///
/// Also installs a panic hook so panics land in the log file with a backtrace instead of only
/// on stderr.
///
/// The returned guard must be kept alive for the whole program, or buffered file writes are lost.
pub fn init() -> Option<WorkerGuard> {
    let stderr_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_filter(stderr_filter);

    let dir = log_dir();
    let file_appender = std::fs::create_dir_all(&dir)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix("meeting-recorder")
                .filename_suffix("log")
                .max_log_files(MAX_LOG_FILES)
                .build(&dir)
                .map_err(|e| e.to_string())
        });

    let (file_layer, guard, file_err) = match file_appender {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let filter = EnvFilter::try_from_env("MEETING_RECORDER_FILE_LOG")
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
            let layer = fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_target(true)
                .with_thread_names(true)
                .with_filter(filter);
            (Some(layer), Some(guard), None)
        }
        Err(e) => (None, None, Some(e)),
    };

    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .init();

    match file_err {
        None => tracing::info!(
            "logging to {:?} (daily rotation, keeping {MAX_LOG_FILES})",
            dir
        ),
        Some(e) => tracing::warn!("file logging disabled, could not open {:?}: {e}", dir),
    }

    install_panic_hook();
    guard
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".into());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("PANIC in thread '{thread}' at {location}: {msg}\nbacktrace:\n{backtrace}");
        default_hook(info);
    }));
}
