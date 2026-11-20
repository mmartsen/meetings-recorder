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
}

/// Starts capturing audio via `parec` (PulseAudio/PipeWire-pulse simple recorder) piped
/// straight into `ffmpeg` for encoding. Using the *default* mic + `@DEFAULT_SINK@.monitor`
/// covers the common "mic + whatever's playing" case without needing to track individual
/// per-app stream indices (which can churn mid-call, e.g. Teams re-opening its audio stream).
pub async fn start(cfg: &RecordingConfig, platform: Option<String>) -> Result<ActiveRecording> {
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

    let mut child = cmd.spawn().context(
        "spawning ffmpeg - make sure ffmpeg is installed and its pulse input device is available",
    )?;

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
    let _ = rec.child.wait().await;
    Ok(rec.output_path)
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
