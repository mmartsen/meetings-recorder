use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Full application configuration, loaded from ~/.config/meeting-recorder/config.toml
/// A default file is written on first run so the user has something to edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub general: GeneralConfig,
    pub detection: DetectionConfig,
    pub recording: RecordingConfig,
    pub pipeline: PipelineConfig,
    pub calendar: CalendarConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// Poll interval fallback (seconds) in case event-driven updates are missed.
    pub poll_interval_secs: u64,
    /// Show a desktop notification whenever recording starts/stops.
    pub notify_on_start_stop: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionConfig {
    /// Process / binary names that count as known meeting apps, mapped to a friendly platform
    /// name. Matched against `application.process.binary` / `application.name` from PipeWire.
    pub known_apps: Vec<AppMatch>,
    /// Browser process names (chrome, firefox, chromium...) - when one of these is the audio
    /// source, we additionally check the active window title against `browser_meeting_titles`.
    pub browser_processes: Vec<String>,
    /// Window-title substrings (case-insensitive) that indicate a browser tab is an active
    /// video/voice call (Meet, Slack huddle, Teams web, etc).
    pub browser_meeting_titles: Vec<TitleMatch>,
    /// Minimum seconds of simultaneous mic + call-app audio before we call it a "meeting"
    /// rather than a brief notification sound / test ping.
    pub min_duration_secs: u64,
    /// Minimum confidence score (0-100) required to auto-start recording.
    pub confidence_threshold: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMatch {
    pub process_match: String, // substring match, case-insensitive
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleMatch {
    pub title_match: String,
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingConfig {
    pub output_dir: PathBuf,
    /// "mic", "call_audio", or "both" (mixed into one file)
    pub capture_mode: String,
    /// ffmpeg container/codec, e.g. "opus" (ogg) or "mp3"
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Enable running post-processing hooks after a recording finishes.
    pub enabled: bool,
    /// Ordered list of hooks. Each is an external command run with:
    ///   <command> <audio_file_path> <metadata_json_path>
    /// so the user can bolt on Whisper, a diarizer, a summarizer, an upload script, etc,
    /// entirely outside this codebase.
    pub hooks: Vec<PipelineHook>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineHook {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// If false, a failing hook is logged but doesn't block later hooks.
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarConfig {
    /// Which calendar backend to use for cross-checking: "none", "google", or "ics".
    /// "ics" reads local .ics calendar files directly (e.g. KOrganizer/Akonadi local
    /// resources, Thunderbird, davical/vdirsyncer folders) - no OAuth or network needed.
    pub provider: String,
    /// Minutes of slack allowed around a calendar event's start/end when matching.
    pub match_window_minutes: i64,
    pub google: GoogleCalendarConfig,
    pub ics: IcsCalendarConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleCalendarConfig {
    /// OAuth2 refresh token for a Google account with Calendar readonly scope.
    /// See README.md "Google Calendar setup" for how to obtain this.
    pub refresh_token: String,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IcsCalendarConfig {
    /// Explicit paths to .ics files (or directories containing them, e.g. a vdir) to read.
    pub paths: Vec<PathBuf>,
    /// If true, also scan Akonadi's own resource configs under ~/.config for local ICal
    /// resources and read whatever .ics files they point at. This is how a plain "point it at
    /// my KDE calendar" setup can work without you having to find the path yourself.
    /// Note: this only sees *local* Akonadi ICal resources - a Google/CalDAV account added
    /// through Akonadi's own sync agents is stored in Akonadi's internal database, not a plain
    /// .ics file, and isn't readable this way (see README "Calendar backends" for details).
    pub auto_discover_kde: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            general: GeneralConfig {
                poll_interval_secs: 5,
                notify_on_start_stop: true,
            },
            detection: DetectionConfig {
                known_apps: vec![
                    AppMatch {
                        process_match: "discord".into(),
                        platform: "Discord".into(),
                    },
                    AppMatch {
                        process_match: "teams".into(),
                        platform: "MS Teams".into(),
                    },
                    AppMatch {
                        process_match: "slack".into(),
                        platform: "Slack".into(),
                    },
                    AppMatch {
                        process_match: "zoom".into(),
                        platform: "Zoom".into(),
                    },
                    AppMatch {
                        process_match: "skypeforlinux".into(),
                        platform: "Skype".into(),
                    },
                ],
                browser_processes: vec![
                    "chrome".into(),
                    "chromium".into(),
                    "firefox".into(),
                    "brave".into(),
                ],
                browser_meeting_titles: vec![
                    TitleMatch {
                        title_match: "meet.google.com".into(),
                        platform: "Google Meet".into(),
                    },
                    TitleMatch {
                        title_match: " | meet".into(),
                        platform: "Google Meet".into(),
                    },
                    // Firefox/Chrome tab title for an active call: "Meet - <meeting name>".
                    TitleMatch {
                        title_match: "meet - ".into(),
                        platform: "Google Meet".into(),
                    },
                    TitleMatch {
                        title_match: "teams.microsoft.com".into(),
                        platform: "MS Teams (web)".into(),
                    },
                    TitleMatch {
                        title_match: "app.slack.com".into(),
                        platform: "Slack (huddle)".into(),
                    },
                ],
                min_duration_secs: 12,
                confidence_threshold: 60,
            },
            recording: RecordingConfig {
                output_dir: default_output_dir(),
                capture_mode: "both".into(),
                format: "opus".into(),
            },
            pipeline: PipelineConfig {
                enabled: false,
                hooks: vec![
                    // Disabled example showing how a user would wire in local Whisper.
                    PipelineHook {
                        name: "whisper-transcribe (example, disabled)".into(),
                        command: "/usr/bin/whisper".into(),
                        args: vec![
                            "--model".into(),
                            "base".into(),
                            "--output_format".into(),
                            "txt".into(),
                        ],
                        required: false,
                    },
                ],
            },
            calendar: CalendarConfig {
                provider: "none".into(),
                match_window_minutes: 10,
                google: GoogleCalendarConfig {
                    refresh_token: String::new(),
                    client_id: String::new(),
                    client_secret: String::new(),
                },
                ics: IcsCalendarConfig {
                    paths: Vec::new(),
                    auto_discover_kde: true,
                },
            },
        }
    }
}

fn default_output_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Recordings")
        .join("meetings")
}

fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("could not determine config dir")?
        .join("meeting-recorder");
    Ok(dir.join("config.toml"))
}

impl Config {
    pub fn load_or_create() -> Result<Config> {
        let path = config_path()?;
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading config at {:?}", path))?;
            let cfg: Config = toml::from_str(&text).context("parsing config.toml")?;
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text)?;
        std::fs::create_dir_all(&self.recording.output_dir)?;
        Ok(())
    }
}
