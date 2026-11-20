use std::process::Command;

/// Returns titles of all open windows belonging to `process_hint` (best-effort substring match),
/// so we can tell whether a browser currently has a meeting tab/window open.
///
/// X11: uses `wmctrl -l` (title only, no per-process filtering available cheaply, so we return
/// all titles and let the caller substring-match).
/// Wayland (GNOME/KDE): no universal API; documented fallback is `wmctrl`/`xdotool` under
/// XWayland, or a compositor-specific extension. We degrade gracefully (empty list) rather than
/// erroring, since browser-tab detection is a "nice to have" on top of the process-based signal.
pub fn all_window_titles() -> Vec<String> {
    if let Ok(out) = Command::new("wmctrl").arg("-l").output() {
        if out.status.success() {
            return String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    // wmctrl -l format: <id> <desktop> <host> <title...>
                    let mut parts = line.splitn(4, char::is_whitespace);
                    parts.nth(3).map(|s| s.trim().to_string())
                })
                .collect();
        }
    }
    Vec::new()
}
