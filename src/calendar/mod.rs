mod google;
mod ics;

use crate::config::CalendarConfig;
use anyhow::Result;

/// Returns the title of a calendar event currently in progress (within `match_window_minutes`
/// of now), if any, using whichever provider is configured. Errors here are logged by the
/// caller and treated as non-fatal - calendar cross-check is a confidence booster, never a
/// requirement for recording to work.
pub async fn current_event_title(cfg: &CalendarConfig) -> Result<Option<String>> {
    match cfg.provider.as_str() {
        "google" => google::current_event_title(&cfg.google, cfg.match_window_minutes).await,
        "ics" => ics::current_event_title(&cfg.ics, cfg.match_window_minutes),
        _ => Ok(None),
    }
}
