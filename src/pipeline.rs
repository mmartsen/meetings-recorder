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
    for hook in &cfg.hooks {
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
                tracing::info!(
                    "hook '{}' completed in {}s",
                    hook.name,
                    started.elapsed().as_secs()
                );
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
                if hook.required {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!("hook '{}' failed to launch: {}", hook.name, e);
                if hook.required {
                    break;
                }
            }
        }
    }
}
