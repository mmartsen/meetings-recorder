use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Command;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;

/// A single active audio stream reported by pactl (either the mic being read by an app,
/// or an app currently outputting sound).
#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub index: String,
    pub app_name: String,
    pub app_binary: String,
}

/// Snapshot of "who is using audio right now".
#[derive(Debug, Clone, Default)]
pub struct AudioSnapshot {
    /// Apps currently reading from the microphone (source-outputs).
    pub mic_users: Vec<StreamInfo>,
    /// Apps currently producing sound (sink-inputs) - includes notification sounds too,
    /// so this alone isn't sufficient signal; combine with mic_users.
    pub audio_producers: Vec<StreamInfo>,
}

/// Runs `pactl list <kind>` and parses out index / application.name / application.process.binary.
/// pactl's plain-text output is stable enough for this across PulseAudio and PipeWire-pulse.
fn list_streams(kind: &str) -> Result<Vec<StreamInfo>> {
    let out = Command::new("pactl")
        .args(["list", kind])
        .output()
        .context("running pactl (is pulseaudio-utils / pipewire-pulse installed?)")?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_pactl_list(&text))
}

fn parse_pactl_list(text: &str) -> Vec<StreamInfo> {
    let mut result = Vec::new();
    let mut cur_index: Option<String> = None;
    let mut props: HashMap<String, String> = HashMap::new();

    let flush = |index: &Option<String>, props: &HashMap<String, String>, out: &mut Vec<StreamInfo>| {
        if let Some(idx) = index {
            let app_name = props.get("application.name").cloned().unwrap_or_default();
            let app_binary = props.get("application.process.binary").cloned().unwrap_or_default();
            if !app_name.is_empty() || !app_binary.is_empty() {
                out.push(StreamInfo { index: idx.clone(), app_name, app_binary });
            }
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Source Output #").or_else(|| trimmed.strip_prefix("Sink Input #")) {
            flush(&cur_index, &props, &mut result);
            cur_index = Some(rest.to_string());
            props.clear();
        } else if let Some((key, val)) = trimmed.split_once(" = ") {
            // property lines look like: application.name = "Discord"
            props.insert(key.to_string(), val.trim_matches('"').to_string());
        }
    }
    flush(&cur_index, &props, &mut result);
    result
}

pub fn snapshot() -> Result<AudioSnapshot> {
    Ok(AudioSnapshot {
        mic_users: list_streams("source-outputs")?,
        audio_producers: list_streams("sink-inputs")?,
    })
}

/// Events we care about from `pactl subscribe`. We don't parse the event payload in detail -
/// any source-output/sink-input change just tells us "go take a fresh snapshot()".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioEvent {
    Changed,
}

/// Spawns `pactl subscribe` and forwards a debounced "something changed" signal.
/// This is far cheaper than polling snapshot() constantly, while still catching every change
/// immediately (subscribe is push-based).
pub async fn watch(tx: mpsc::Sender<AudioEvent>) -> Result<()> {
    let mut child = TokioCommand::new("pactl")
        .args(["subscribe"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("spawning `pactl subscribe` - is pactl on PATH?")?;

    let stdout = child.stdout.take().context("no stdout from pactl subscribe")?;
    let mut lines = BufReader::new(stdout).lines();

    while let Some(line) = lines.next_line().await? {
        if line.contains("source-output") || line.contains("sink-input") {
            // Best-effort send; if the channel is full a change notification is already queued.
            let _ = tx.try_send(AudioEvent::Changed);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pactl_output() {
        let sample = r#"
Source Output #142
	Driver: PipeWire
	Owner Module: n/a
	Client: 87
	Source: 55
	Sample Specification: float32le 2ch 48000Hz
	Properties:
		application.name = "Discord"
		application.process.binary = "Discord"
		media.name = "input"

Source Output #150
	Properties:
		application.name = "Firefox"
		application.process.binary = "firefox"
"#;
        let parsed = parse_pactl_list(sample);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].app_name, "Discord");
        assert_eq!(parsed[1].app_binary, "firefox");
    }
}
