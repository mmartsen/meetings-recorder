use crate::config::RecordingConfig;
use anyhow::{Context, Result};
use chrono::Local;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

pub struct ActiveRecording {
    pub child: Child,
    pub output_path: PathBuf,
    pub started_at: chrono::DateTime<Local>,
    pub platform: Option<String>,
    /// Detection confidence at the moment recording started (what goes into metadata).
    pub confidence: u8,
}

/// Starts capturing audio via `parec` (PulseAudio/PipeWire-pulse simple recorder) piped
/// straight into `ffmpeg` for encoding. Using the *default* mic + `@DEFAULT_SINK@.monitor`
/// covers the common "mic + whatever's playing" case without needing to track individual
/// per-app stream indices (which can churn mid-call, e.g. Teams re-opening its audio stream).
pub async fn start(
    cfg: &RecordingConfig,
    platform: Option<String>,
    confidence: u8,
) -> Result<ActiveRecording> {
    std::fs::create_dir_all(&cfg.output_dir)?;

    let started_at = Local::now();
    let label = platform.clone().unwrap_or_else(|| "unknown".to_string());
    let filename = format!(
        "{}_{}.{}",
        started_at.format("%Y-%m-%d_%H-%M-%S"),
        sanitize(&label),
        container_ext(&cfg.format)
    );
    let output_path = cfg.output_dir.join(filename);

    // Build the ffmpeg command depending on capture_mode. We use pulse's ffmpeg input device
    // (works against PipeWire's pulse-compat layer too) rather than shelling through parec, so
    // we only need one child process to manage instead of piping two together.
    let mut cmd = Command::new("ffmpeg");
    // -nostats/-loglevel warning keep stderr quiet (no per-second progress lines), so the log
    // isn't flooded; stderr is still drained below regardless.
    cmd.args(["-y", "-hide_banner", "-nostats", "-loglevel", "warning"]);

    match cfg.capture_mode.as_str() {
        "mic" => {
            cmd.args(["-f", "pulse", "-i", "default"]);
        }
        "call_audio" => {
            cmd.args(["-f", "pulse", "-i", "@DEFAULT_SINK@.monitor"]);
        }
        _ => {
            // "both" - two inputs mixed with amix so you get one file with both sides audible.
            cmd.args(["-f", "pulse", "-i", "default"]);
            cmd.args(["-f", "pulse", "-i", "@DEFAULT_SINK@.monitor"]);
            cmd.args([
                "-filter_complex",
                "amix=inputs=2:duration=longest:dropout_transition=2",
            ]);
        }
    }

    match cfg.format.as_str() {
        "mp3" => cmd.args(["-c:a", "libmp3lame", "-q:a", "4"]),
        _ => cmd.args(["-c:a", "libopus", "-b:a", "48k"]),
    };
    cmd.arg(&output_path);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    let cmd_line = format!("{:?}", cmd.as_std());
    tracing::debug!("spawning recorder: {cmd_line}");
    let mut child = cmd.spawn().context(
        "spawning ffmpeg - make sure ffmpeg is installed and its pulse input device is available",
    )?;
    tracing::info!(
        "ffmpeg started (pid {:?}, capture_mode={}, format={}) -> {:?}",
        child.id(),
        cfg.capture_mode,
        cfg.format,
        output_path
    );

    // stderr is piped, so it MUST be drained continuously: if nobody reads it, the ~64KB pipe
    // buffer fills and ffmpeg blocks on write, stalling the recording. Forward each line to
    // tracing until EOF (which happens when ffmpeg exits).
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let lower = line.to_ascii_lowercase();
                        if lower.contains("error") || lower.contains("warning") {
                            tracing::warn!("ffmpeg: {line}");
                        } else {
                            tracing::debug!("ffmpeg: {line}");
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        // Invalid UTF-8 etc. Keep draining raw bytes so the pipe never fills.
                        tracing::debug!("ffmpeg stderr read error: {e}; draining remainder");
                        let mut rest = lines.into_inner();
                        let _ = tokio::io::copy(&mut rest, &mut tokio::io::sink()).await;
                        break;
                    }
                }
            }
        });
    }

    Ok(ActiveRecording {
        child,
        output_path,
        started_at,
        platform,
        confidence,
    })
}

/// Stops the recording gracefully (SIGINT-equivalent via ffmpeg's stdin 'q' doesn't apply since
/// stdin is null here, so we kill and rely on ffmpeg having flushed periodically; for stricter
/// setups swap to a real pipe and send `q`).
pub async fn stop(mut rec: ActiveRecording) -> Result<PathBuf> {
    // Send SIGTERM first so ffmpeg finalizes the container properly instead of leaving a
    // corrupt file from a hard kill.
    if let Some(id) = rec.child.id() {
        unsafe {
            libc::kill(id as i32, libc::SIGTERM);
        }
    }
    let status = rec
        .child
        .wait()
        .await
        .context("waiting for ffmpeg to exit")?;
    let size = std::fs::metadata(&rec.output_path).map(|m| m.len()).ok();
    match size {
        Some(0) | None => tracing::warn!(
            "ffmpeg exited ({status}) but output {:?} is missing or empty (size {size:?})",
            rec.output_path
        ),
        Some(bytes) => tracing::info!(
            "ffmpeg exited ({status}), wrote {bytes} bytes to {:?}",
            rec.output_path
        ),
    }
    Ok(rec.output_path)
}

/// Returns `Some(status)` if ffmpeg has already exited on its own (crashed, device vanished...),
/// which means we're "recording" nothing. The caller should log and drop the recording.
pub fn exited_early(rec: &mut ActiveRecording) -> Option<std::process::ExitStatus> {
    rec.child.try_wait().ok().flatten()
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

fn container_ext(format: &str) -> &'static str {
    match format {
        "mp3" => "mp3",
        _ => "opus",
    }
}
