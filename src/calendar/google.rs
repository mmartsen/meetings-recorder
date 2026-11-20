use crate::config::GoogleCalendarConfig;
use anyhow::{bail, Context, Result};
use chrono::{Duration, Local, Utc};
use serde::Deserialize;
use tokio::process::Command;

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct EventsResponse {
    #[serde(default)]
    items: Vec<EventItem>,
}

#[derive(Debug, Deserialize)]
struct EventItem {
    summary: Option<String>,
    start: EventTime,
    end: EventTime,
}

#[derive(Debug, Deserialize)]
struct EventTime {
    #[serde(rename = "dateTime")]
    date_time: Option<String>,
}

/// Runs curl and returns stdout as a String. Using curl (rather than an HTTP client crate)
/// keeps this app's dependency tree small - this backend only needs two simple JSON requests.
async fn curl(args: &[&str]) -> Result<String> {
    let out = Command::new("curl").args(args).output().await.context("spawning curl")?;
    if !out.status.success() {
        bail!("curl exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Exchanges the stored refresh token for a short-lived access token.
/// See README.md "Google Calendar setup" for how to obtain client_id/client_secret/refresh_token
/// once, up front (standard Google OAuth "installed app" flow, read-only Calendar scope).
async fn get_access_token(cfg: &GoogleCalendarConfig) -> Result<String> {
    let body = curl(&[
        "-s",
        "-X",
        "POST",
        "https://oauth2.googleapis.com/token",
        "-d",
        &format!("client_id={}", cfg.client_id),
        "-d",
        &format!("client_secret={}", cfg.client_secret),
        "-d",
        &format!("refresh_token={}", cfg.refresh_token),
        "-d",
        "grant_type=refresh_token",
    ])
    .await?;
    let resp: TokenResponse = serde_json::from_str(&body).context("parsing Google OAuth token response")?;
    Ok(resp.access_token)
}

pub async fn current_event_title(cfg: &GoogleCalendarConfig, match_window_minutes: i64) -> Result<Option<String>> {
    if cfg.refresh_token.is_empty() {
        return Ok(None);
    }
    let token = get_access_token(cfg).await?;
    let now = Utc::now();
    let window = Duration::minutes(match_window_minutes);
    let time_min = (now - window).to_rfc3339();
    let time_max = (now + window).to_rfc3339();

    let url = format!(
        "https://www.googleapis.com/calendar/v3/calendars/primary/events?timeMin={}&timeMax={}&singleEvents=true&orderBy=startTime",
        urlencode(&time_min),
        urlencode(&time_max)
    );
    let body = curl(&["-s", "-H", &format!("Authorization: Bearer {token}"), &url]).await?;
    let resp: EventsResponse = serde_json::from_str(&body).context("parsing Google Calendar events response")?;

    let now_local = Local::now();
    for event in resp.items {
        if let (Some(start), Some(end)) = (event.start.date_time, event.end.date_time) {
            if let (Ok(start), Ok(end)) = (
                chrono::DateTime::parse_from_rfc3339(&start),
                chrono::DateTime::parse_from_rfc3339(&end),
            ) {
                let start = start.with_timezone(&Local);
                let end = end.with_timezone(&Local);
                if now_local >= start - window && now_local <= end + window {
                    return Ok(event.summary);
                }
            }
        }
    }
    Ok(None)
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}
