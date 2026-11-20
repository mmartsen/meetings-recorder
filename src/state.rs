use crate::stats::StatsLog;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub type SharedState = Arc<Mutex<AppState>>;

/// Manual override set from the tray's "Force start/stop" button. `None` means "let the
/// automatic detector decide"; `Some(true/false)` pins the state until toggled again.
pub struct AppState {
    pub recording: bool,
    pub current_platform: Option<String>,
    pub force_override: Option<bool>,
    pub stats: StatsLog,
    pub output_dir: PathBuf,
}

impl AppState {
    pub fn new(output_dir: PathBuf) -> Self {
        AppState {
            recording: false,
            current_platform: None,
            force_override: None,
            stats: StatsLog::load().unwrap_or_default(),
            output_dir,
        }
    }

    /// Called by the tray click handler. Toggling sets an override that the main loop honors
    /// until the user toggles it again (returning control to automatic detection would need a
    /// third click / menu item in a fuller UI - kept simple here on purpose).
    pub fn toggle_force_override(&mut self) {
        let new_override = !self.recording;
        self.force_override = Some(new_override);
    }
}
