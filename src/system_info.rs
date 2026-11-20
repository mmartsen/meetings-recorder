use std::process::Command;

/// A single external tool this app shells out to somewhere in its pipeline.
struct ToolCheck {
    binary: &'static str,
    required: bool,
    used_for: &'static str,
}

const TOOLS: &[ToolCheck] = &[
    ToolCheck {
        binary: "pactl",
        required: true,
        used_for: "mic/app audio detection",
    },
    ToolCheck {
        binary: "ffmpeg",
        required: true,
        used_for: "recording",
    },
    ToolCheck {
        binary: "wmctrl",
        required: false,
        used_for: "browser-tab meeting detection (Meet/Slack/Teams web)",
    },
    ToolCheck {
        binary: "curl",
        required: false,
        used_for: "Google Calendar cross-check",
    },
];

fn which(binary: &str) -> bool {
    Command::new("which")
        .arg(binary)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Logs a snapshot of the desktop environment this instance is running under, plus which of
/// the external tools it depends on are actually available. Run once at startup so any
/// missing-dependency or "why doesn't detection work on my compositor" question can be
/// answered by scrolling up in the logs instead of guessing.
pub fn log_system_info() {
    let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .unwrap_or_else(|_| "unknown".into());
    let session_desktop = std::env::var("XDG_SESSION_DESKTOP").unwrap_or_else(|_| "unknown".into());
    let has_wayland_display = std::env::var("WAYLAND_DISPLAY").is_ok();
    let has_x_display = std::env::var("DISPLAY").is_ok();

    let inferred_session = if session_type != "unknown" {
        session_type.clone()
    } else if has_wayland_display {
        "wayland (inferred from WAYLAND_DISPLAY)".to_string()
    } else if has_x_display {
        "x11 (inferred from DISPLAY)".to_string()
    } else {
        "unknown".to_string()
    };

    tracing::info!(
        "desktop environment: {} (session desktop: {}), session type: {}",
        desktop,
        session_desktop,
        inferred_session
    );

    if inferred_session.starts_with("wayland") {
        tracing::info!(
            "running under Wayland - window-title based browser-meeting detection (wmctrl) \
             typically only sees XWayland windows, not native Wayland ones; native-app \
             detection (Discord/Teams/Slack/Zoom via pactl) is unaffected"
        );
    }

    for tool in TOOLS {
        let present = which(tool.binary);
        if present {
            tracing::info!(
                "dependency check: '{}' found (used for: {})",
                tool.binary,
                tool.used_for
            );
        } else if tool.required {
            tracing::warn!(
                "dependency check: REQUIRED tool '{}' not found on PATH - {} will not work until it's installed",
                tool.binary,
                tool.used_for
            );
        } else {
            tracing::info!(
                "dependency check: optional tool '{}' not found on PATH - {} will be unavailable",
                tool.binary,
                tool.used_for
            );
        }
    }

    let akonadi_running =
        which("akonadi_control") || std::path::Path::new(&akonadi_data_dir()).exists();
    tracing::info!(
        "Akonadi (KDE PIM storage) detected: {} - relevant only if calendar.provider = \"ics\" with auto_discover_kde = true",
        akonadi_running
    );
}

fn akonadi_data_dir() -> String {
    dirs::data_dir()
        .map(|d| d.join("akonadi").to_string_lossy().to_string())
        .unwrap_or_default()
}
