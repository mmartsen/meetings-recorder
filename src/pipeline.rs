use crate::config::PipelineConfig;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Metadata written alongside each recording as `<file>.json`, so pipeline hooks (or anything
/// else you point at the output folder later) can pick up structured context without re-deriving
/// it from the filename.
#[derive(Debug, Serialize)]
pub struct RecordingMetadata {
    pub started_at: chrono::DateTime<chrono::Local>,
    pub ended_at: chrono::DateTime<chrono::Local>,
    pub duration_secs: i64,
    pub platform: Option<String>,
    pub confidence: u8,
    /// Populated only if calendar cross-check was enabled and matched an event.
    pub calendar_event_title: Option<String>,
    pub audio_file: PathBuf,
}

pub fn write_metadata(path: &Path, meta: &RecordingMetadata) -> anyhow::Result<PathBuf> {
    let meta_path = path.with_extension(format!(
        "{}.json",
        path.extension().and_then(|e| e.to_str()).unwrap_or("audio")
    ));
    let json = serde_json::to_string_pretty(meta)?;
    std::fs::write(&meta_path, json)?;
    Ok(meta_path)
}

/// Runs each configured hook in order: `<command> <args...> <audio_file> <metadata_json>`.
/// This is the extension point for Whisper transcription, diarization, summarization, upload,
/// etc - all implemented outside this app as independent scripts/binaries.
pub async fn run_hooks(cfg: &PipelineConfig, audio_file: &Path, metadata_file: &Path) {
    if !cfg.enabled {
        return;
    }
    let file_name = audio_file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    for (i, hook) in cfg.hooks.iter().enumerate() {
        let remaining = cfg.hooks.len() - i - 1;
        tracing::info!(
            "running pipeline hook '{}' on {:?}",
            hook.name,
            audio_file.file_name().unwrap_or_default()
        );
        let started = std::time::Instant::now();
        let result = Command::new(&hook.command)
            .args(&hook.args)
            .arg(audio_file)
            .arg(metadata_file)
            .output()
            .await;

        match result {
            Ok(out) if out.status.success() => {
                let secs = started.elapsed().as_secs();
                tracing::info!("hook '{}' completed in {}s", hook.name, secs);
                if cfg.notify {
                    crate::notify_with_urgency(
                        &format!("{} done", hook.name),
                        &format!("{file_name} ({})", fmt_duration(secs)),
                        notify_rust::Urgency::Normal,
                    )
                    .await;
                }
            }
            Ok(out) => {
                let msg = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(
                    "hook '{}' exited non-zero ({}) after {}s: {}",
                    hook.name,
                    out.status,
                    started.elapsed().as_secs(),
                    msg.trim()
                );
                if cfg.notify {
                    let detail = last_lines(&msg, 3);
                    let detail = if detail.is_empty() {
                        out.status.to_string()
                    } else {
                        detail
                    };
                    notify_failure(&hook.name, &file_name, &detail, hook.required, remaining)
                        .await;
                }
                if hook.required {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!("hook '{}' failed to launch: {}", hook.name, e);
                if cfg.notify {
                    let detail = format!("failed to launch {}: {e}", hook.command);
                    notify_failure(&hook.name, &file_name, &detail, hook.required, remaining)
                        .await;
                }
                if hook.required {
                    break;
                }
            }
        }
    }
}

async fn notify_failure(hook: &str, file: &str, detail: &str, required: bool, remaining: usize) {
    let mut body = format!("{file}\n{detail}");
    if required && remaining > 0 {
        body.push_str(&format!("\nSkipped {remaining} later hook(s)."));
    }
    crate::notify_with_urgency(&format!("{hook} failed"), &body, notify_rust::Urgency::Critical)
        .await;
}

/// Last `n` non-empty lines of a hook's stderr: the error is usually at the end, after
/// progress output.
fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn fmt_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_lines_skips_blank_and_keeps_tail() {
        assert_eq!(last_lines("a\n\nb\nc\n  \nd\n", 2), "c\nd");
        assert_eq!(last_lines("", 3), "");
    }

    #[test]
    fn formats_duration() {
        assert_eq!(fmt_duration(42), "42s");
        assert_eq!(fmt_duration(125), "2m 05s");
    }
}
