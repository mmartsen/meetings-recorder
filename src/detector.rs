use crate::audio::AudioSnapshot;
use crate::config::DetectionConfig;
use crate::window;

#[derive(Debug, Clone)]
pub struct Detection {
    pub platform: Option<String>,
    /// 0-100 confidence that this is a genuine meeting, not a ping/test sound/music.
    pub confidence: u8,
    pub reasons: Vec<String>,
}

/// Core heuristic: is a mic active AND paired with a known meeting app's audio, or a browser
/// with a meeting-shaped tab title? Confidence is additive across independent signals so it's
/// easy to reason about and tune via config.
pub fn detect(snapshot: &AudioSnapshot, cfg: &DetectionConfig) -> Detection {
    let mut confidence: i32 = 0;
    let mut reasons = Vec::new();
    let mut platform: Option<String> = None;

    let mic_active = !snapshot.mic_users.is_empty();
    if !mic_active {
        return Detection {
            platform: None,
            confidence: 0,
            reasons: vec!["mic inactive".into()],
        };
    }
    confidence += 20;
    reasons.push("microphone active".into());

    // Signal 1: a known native meeting app (Discord/Teams/Slack/Zoom) is producing audio.
    for producer in &snapshot.audio_producers {
        for known in &cfg.known_apps {
            if matches_app(producer, &known.process_match) {
                platform = Some(known.platform.clone());
                confidence += 60;
                reasons.push(format!("known app '{}' producing audio", known.platform));
            }
        }
    }

    // Signal 1b: the mic itself is being read by a known app (covers apps that only use mic,
    // e.g. push-to-talk tools, or when the sink-input hasn't started yet).
    if platform.is_none() {
        for user in &snapshot.mic_users {
            for known in &cfg.known_apps {
                if matches_app(user, &known.process_match) {
                    platform = Some(known.platform.clone());
                    confidence += 50;
                    reasons.push(format!("known app '{}' reading microphone", known.platform));
                }
            }
        }
    }

    // Signal 2: a browser is the audio source/sink AND its tab looks like a meeting. The tab
    // title comes from the browser's own audio stream (`media.name`, works on Wayland) plus
    // window titles from wmctrl (X11/XWayland only).
    if platform.is_none() {
        let browser_streams: Vec<&crate::audio::StreamInfo> = snapshot
            .mic_users
            .iter()
            .chain(snapshot.audio_producers.iter())
            .filter(|s| cfg.browser_processes.iter().any(|b| matches_app(s, b)))
            .collect();

        if !browser_streams.is_empty() {
            confidence += 10;
            reasons.push("browser is using audio".into());
            let mut titles: Vec<String> = browser_streams
                .iter()
                .map(|s| s.media_name.clone())
                .filter(|t| !t.is_empty())
                .collect();
            titles.extend(window::all_window_titles());
            'outer: for title in &titles {
                let lower = normalize_title(title);
                for tm in &cfg.browser_meeting_titles {
                    if lower.contains(&normalize_title(&tm.title_match)) {
                        platform = Some(tm.platform.clone());
                        confidence += 40;
                        reasons.push(format!(
                            "title {:?} matched '{}' ({})",
                            title, tm.title_match, tm.platform
                        ));
                        break 'outer;
                    }
                }
            }
        }
    }

    // Signal 3: mic AND some app producing audio simultaneously (two-way audio), even without
    // a recognized platform - still worth a small bump, since it rules out "mic active while
    // scrolling silently".
    if !snapshot.audio_producers.is_empty() {
        confidence += 5;
    }

    Detection {
        platform,
        confidence: confidence.clamp(0, 100) as u8,
        reasons,
    }
}

/// Lowercase and fold dash variants to ASCII '-': Meet's tab title switched from
/// "Meet - X" to "Meet – X" (en-dash), which silently broke the "meet - " pattern.
fn normalize_title(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            c => c,
        })
        .collect::<String>()
        .to_lowercase()
}

fn matches_app(stream: &crate::audio::StreamInfo, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    stream.app_name.to_lowercase().contains(&needle)
        || stream.app_binary.to_lowercase().contains(&needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioSnapshot, StreamInfo};
    use crate::config::Config;

    fn ff(media: &str) -> StreamInfo {
        StreamInfo {
            app_name: "Firefox".into(),
            app_binary: "firefox".into(),
            media_name: media.into(),
            pid: None,
        }
    }

    #[test]
    fn detects_meet_from_browser_stream_media_name() {
        let cfg = Config::default().detection;
        let snap = AudioSnapshot {
            mic_users: vec![ff("Meet - Finance & Data Catch up")],
            audio_producers: vec![ff("Meet - Finance & Data Catch up")],
        };
        let d = detect(&snap, &cfg);
        assert_eq!(d.platform.as_deref(), Some("Google Meet"));
        assert!(d.confidence >= cfg.confidence_threshold, "{d:?}");
    }

    #[test]
    fn detects_meet_with_en_dash_title() {
        let snap = AudioSnapshot {
            mic_users: vec![ff("Meet \u{2013} Weekly Sync")],
            audio_producers: vec![ff("Meet \u{2013} Weekly Sync")],
        };
        let cfg = Config::default().detection;
        let d = detect(&snap, &cfg);
        assert_eq!(d.platform.as_deref(), Some("Google Meet"));
        assert!(d.confidence >= cfg.confidence_threshold, "{d:?}");
    }

    #[test]
    fn browser_without_meeting_tab_stays_below_threshold() {
        let cfg = Config::default().detection;
        let snap = AudioSnapshot {
            mic_users: vec![ff("YouTube")],
            audio_producers: vec![ff("YouTube")],
        };
        let d = detect(&snap, &cfg);
        assert!(d.confidence < cfg.confidence_threshold, "{d:?}");
    }
}
