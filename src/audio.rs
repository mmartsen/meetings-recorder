use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Command;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;

/// A single active audio stream reported by pactl (either the mic being read by an app,
/// or an app currently outputting sound).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub app_name: String,
    pub app_binary: String,
    /// `media.name` - for browsers this is usually the tab title (e.g. "Meet - Standup"),
    /// which works on Wayland where window titles are not readable.
    pub media_name: String,
    /// `application.process.id`, used to drop our own ffmpeg's capture streams.
    pub pid: Option<u32>,
}

/// Snapshot of "who is using audio right now".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    // A failed pactl (e.g. pipewire-pulse restarting, or the service started before the audio
    // server was up) prints nothing on stdout. Without this check it would be indistinguishable
    // from "nobody is using the mic" and silently suppress detection.
    if !out.status.success() {
        anyhow::bail!(
            "`pactl list {kind}` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_pactl_list(&text))
}

fn parse_pactl_list(text: &str) -> Vec<StreamInfo> {
    let mut result = Vec::new();
    let mut cur_index: Option<String> = None;
    let mut props: HashMap<String, String> = HashMap::new();

    let flush =
        |index: &Option<String>, props: &HashMap<String, String>, out: &mut Vec<StreamInfo>| {
            if index.is_some() {
                let app_name = props.get("application.name").cloned().unwrap_or_default();
                let app_binary = props
                    .get("application.process.binary")
                    .cloned()
                    .unwrap_or_default();
                let media_name = props.get("media.name").cloned().unwrap_or_default();
                let pid = props
                    .get("application.process.id")
                    .and_then(|p| p.parse().ok());
                if !app_name.is_empty() || !app_binary.is_empty() {
                    out.push(StreamInfo {
                        app_name,
                        app_binary,
                        media_name,
                        pid,
                    });
                }
            }
        };

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("Source Output #")
            .or_else(|| trimmed.strip_prefix("Sink Input #"))
        {
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

impl std::fmt::Display for StreamInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}[{}]", self.app_name, self.app_binary)?;
        if !self.media_name.is_empty() {
            write!(f, "{{{:?}}}", self.media_name)?;
        }
        Ok(())
    }
}

impl std::fmt::Display for AudioSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let join = |v: &[StreamInfo]| {
            v.iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        write!(
            f,
            "mic_users=[{}] audio_producers=[{}]",
            join(&self.mic_users),
            join(&self.audio_producers)
        )
    }
}

/// Current audio streams, excluding the ones our own recorder (an ffmpeg child process of
/// this app) creates. Otherwise the recording itself counts as "mic active" and inflates
/// confidence for as long as it runs.
pub fn snapshot() -> Result<AudioSnapshot> {
    let me = std::process::id();
    let not_ours = |s: &StreamInfo| s.pid.is_none_or(|pid| parent_pid(pid) != Some(me));
    let mut mic_users = list_streams("source-outputs")?;
    let mut audio_producers = list_streams("sink-inputs")?;
    mic_users.retain(not_ours);
    audio_producers.retain(not_ours);
    Ok(AudioSnapshot {
        mic_users,
        audio_producers,
    })
}

/// Parent PID from `/proc/<pid>/stat`. The comm field (2nd) is parenthesised and may contain
/// spaces, so parse from the last ')'.
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_ppid(&stat)
}

fn parse_ppid(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // After comm: state, ppid, ...
    rest.split_whitespace().nth(1)?.parse().ok()
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
/// Runs [`watch_once`] forever, restarting `pactl subscribe` with backoff whenever it exits
/// (it does exit when pipewire-pulse restarts). Without this the app silently falls back to
/// poll-only detection for the rest of its lifetime.
pub async fn watch_forever(tx: mpsc::Sender<AudioEvent>) {
    let mut backoff = std::time::Duration::from_secs(1);
    loop {
        let started = std::time::Instant::now();
        match watch_once(&tx).await {
            Ok(()) => tracing::warn!("`pactl subscribe` exited (EOF); restarting"),
            Err(e) => tracing::error!("`pactl subscribe` failed: {e:#}; restarting"),
        }
        if tx.is_closed() {
            tracing::warn!("audio event receiver dropped; stopping watcher");
            return;
        }
        // Reset backoff if it ran fine for a while; otherwise grow it up to 30s.
        if started.elapsed() > std::time::Duration::from_secs(60) {
            backoff = std::time::Duration::from_secs(1);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
    }
}

async fn watch_once(tx: &mpsc::Sender<AudioEvent>) -> Result<()> {
    let mut child = TokioCommand::new("pactl")
        .args(["subscribe"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawning `pactl subscribe` - is pactl on PATH?")?;
    tracing::info!(
        "audio watcher started (`pactl subscribe` pid {:?})",
        child.id()
    );

    let stdout = child
        .stdout
        .take()
        .context("no stdout from pactl subscribe")?;
    let mut lines = BufReader::new(stdout).lines();

    while let Some(line) = lines.next_line().await? {
        if line.contains("source-output") || line.contains("sink-input") {
            tracing::trace!("pactl event: {line}");
            // Best-effort send; if the channel is full a change notification is already queued.
            let _ = tx.try_send(AudioEvent::Changed);
        }
    }
    let status = child.wait().await;
    tracing::debug!("`pactl subscribe` exit status: {status:?}");
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
		application.process.id = "4242"
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
        assert_eq!(parsed[0].media_name, "input");
        assert_eq!(parsed[0].pid, Some(4242));
        assert_eq!(parsed[1].pid, None);
    }

    #[test]
    fn parses_ppid_with_spaces_in_comm() {
        assert_eq!(
            parse_ppid("123 (my (weird) proc) S 456 123 123 0"),
            Some(456)
        );
        assert_eq!(parse_ppid("garbage"), None);
    }

    #[test]
    fn own_child_is_detected_as_child() {
        let child = std::process::Command::new("sleep")
            .arg("2")
            .spawn()
            .unwrap();
        assert_eq!(parent_pid(child.id()), Some(std::process::id()));
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
    }
}
