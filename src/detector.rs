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

    // Signal 2: a browser is the audio source/sink AND its window title looks like a meeting.
    if platform.is_none() {
        let browser_active = snapshot
            .mic_users
            .iter()
            .chain(snapshot.audio_producers.iter())
            .any(|s| cfg.browser_processes.iter().any(|b| matches_app(s, b)));

        if browser_active {
            confidence += 10;
            reasons.push("browser is using audio".into());
            let titles = window::all_window_titles();
            for title in &titles {
                let lower = title.to_lowercase();
                for tm in &cfg.browser_meeting_titles {
                    if lower.contains(&tm.title_match.to_lowercase()) {
                        platform = Some(tm.platform.clone());
                        confidence += 40;
                        reasons.push(format!(
                            "window title matched '{}' ({})",
                            tm.title_match, tm.platform
                        ));
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

fn matches_app(stream: &crate::audio::StreamInfo, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    stream.app_name.to_lowercase().contains(&needle)
        || stream.app_binary.to_lowercase().contains(&needle)
}
