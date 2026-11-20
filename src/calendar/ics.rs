use crate::config::IcsCalendarConfig;
use anyhow::Result;
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use std::path::{Path, PathBuf};

struct ParsedEvent {
    summary: Option<String>,
    start: chrono::DateTime<Local>,
    end: chrono::DateTime<Local>,
}

/// Returns the title of an event in any configured .ics source that currently covers "now"
/// (within `match_window_minutes`). Every I/O error along the way (missing file, unparsable
/// event) is skipped rather than propagated - a calendar cross-check should degrade quietly,
/// not break recording.
pub fn current_event_title(
    cfg: &IcsCalendarConfig,
    match_window_minutes: i64,
) -> Result<Option<String>> {
    let window = Duration::minutes(match_window_minutes);
    let now = Local::now();

    let mut files = Vec::new();
    for p in &cfg.paths {
        collect_ics_files(p, &mut files);
    }
    if cfg.auto_discover_kde {
        for p in discover_akonadi_ical_paths() {
            collect_ics_files(&p, &mut files);
        }
    }

    for file in files {
        if let Ok(text) = std::fs::read_to_string(&file) {
            for ev in parse_events(&text) {
                if now >= ev.start - window && now <= ev.end + window {
                    return Ok(ev.summary);
                }
            }
        }
    }
    Ok(None)
}

fn collect_ics_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("ics") {
                    out.push(p);
                }
            }
        }
    } else if path.is_file() {
        out.push(path.to_path_buf());
    }
}

/// Scans ~/.config for Akonadi's local ICal-resource config files and pulls out the `Path=`
/// value each one points at. This covers the common "I added a local .ics calendar in
/// KOrganizer" setup with zero extra configuration.
///
/// Does NOT cover a Google/CalDAV/Exchange account added through Akonadi's own sync agents -
/// those are stored in Akonadi's internal database (not a flat .ics file) and reading them
/// would require linking against KDE Frameworks/Qt, which this lightweight Rust daemon
/// deliberately avoids. If that's your setup, either add a second, separate local ICS/vdir
/// calendar in KOrganizer for this tool to read, or use the `google` provider directly.
fn discover_akonadi_ical_paths() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Some(config_dir) = dirs::config_dir() else {
        return found;
    };
    let Ok(entries) = std::fs::read_dir(&config_dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("akonadi_ical_resource_")
            || name.starts_with("akonadi_icaldir_resource_")
        {
            let path = entry.path();
            let rc_file = if path.is_dir() {
                path.join(format!("{name}rc"))
            } else {
                path
            };
            if let Ok(text) = std::fs::read_to_string(&rc_file) {
                for line in text.lines() {
                    if let Some(val) = line.strip_prefix("Path=") {
                        found.push(PathBuf::from(val.trim()));
                    }
                }
            }
        }
    }
    found
}

/// Minimal RFC5545 VEVENT parser: unfolds continuation lines, then extracts SUMMARY/DTSTART/DTEND
/// from each BEGIN:VEVENT..END:VEVENT block. This is deliberately not a full ICS implementation -
/// just enough to answer "is there an event covering right now".
fn parse_events(text: &str) -> Vec<ParsedEvent> {
    let unfolded = unfold(text);
    let mut events = Vec::new();
    let mut in_event = false;
    let mut summary = None;
    let mut start = None;
    let mut end = None;

    for line in unfolded.lines() {
        let line = line.trim_end_matches('\r');
        if line == "BEGIN:VEVENT" {
            in_event = true;
            summary = None;
            start = None;
            end = None;
        } else if line == "END:VEVENT" {
            if in_event {
                if let (Some(s), Some(e)) = (start, end) {
                    events.push(ParsedEvent {
                        summary: summary.clone(),
                        start: s,
                        end: e,
                    });
                }
            }
            in_event = false;
        } else if in_event {
            if let Some((key_part, value)) = line.split_once(':') {
                let key = key_part.split(';').next().unwrap_or("");
                match key {
                    "SUMMARY" => summary = Some(unescape(value)),
                    "DTSTART" => start = parse_datetime(key_part, value),
                    "DTEND" => end = parse_datetime(key_part, value),
                    _ => {}
                }
            }
        }
    }
    events
}

/// Un-folds RFC5545 line folding: a line starting with a space or tab is a continuation of the
/// previous line.
fn unfold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for raw_line in text.split('\n') {
        let raw_line = raw_line.trim_end_matches('\r');
        if (raw_line.starts_with(' ') || raw_line.starts_with('\t')) && !out.is_empty() {
            out.push_str(raw_line.trim_start_matches([' ', '\t']));
        } else {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(raw_line);
        }
    }
    out
}

fn unescape(v: &str) -> String {
    v.replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\n", " ")
        .replace("\\\\", "\\")
}

/// Parses DTSTART/DTEND values: UTC ("Z" suffix), floating/local naive datetimes, and
/// VALUE=DATE all-day events. Non-UTC TZID values are treated as already being in the system's
/// local timezone (no timezone database lookup) - a reasonable approximation for a same-machine
/// meeting-detection heuristic, but worth knowing about if your calendar mixes timezones.
fn parse_datetime(key_part: &str, value: &str) -> Option<chrono::DateTime<Local>> {
    let value = value.trim();
    if key_part.contains("VALUE=DATE") && !key_part.contains("VALUE=DATE-TIME") {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Local
            .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
            .single();
    }
    if let Some(stripped) = value.strip_suffix('Z') {
        let naive = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S").ok()?;
        return Some(Utc.from_utc_datetime(&naive).with_timezone(&Local));
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    Local.from_local_datetime(&naive).single()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_vevent() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Standup\r\nDTSTART:20260101T090000Z\r\nDTEND:20260101T093000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let events = parse_events(ics);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary.as_deref(), Some("Standup"));
    }

    #[test]
    fn unfolds_continuation_lines() {
        let ics = "BEGIN:VEVENT\r\nSUMMARY:Long meeting \r\n title continued\r\nDTSTART:20260101T090000Z\r\nDTEND:20260101T093000Z\r\nEND:VEVENT\r\n";
        let events = parse_events(ics);
        assert_eq!(
            events[0].summary.as_deref(),
            Some("Long meeting title continued")
        );
    }
}
