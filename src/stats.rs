use anyhow::Result;
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecordingLogEntry {
    pub date: NaiveDate,
    pub platform: Option<String>,
    pub duration_secs: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatsLog {
    pub entries: Vec<RecordingLogEntry>,
}

fn log_path() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("no data dir"))?
        .join("meeting-recorder");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("stats.json"))
}

impl StatsLog {
    pub fn load() -> Result<StatsLog> {
        let path = log_path()?;
        if !path.exists() {
            return Ok(StatsLog::default());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    pub fn save(&self) -> Result<()> {
        let path = log_path()?;
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn record(&mut self, platform: Option<String>, duration_secs: i64) -> Result<()> {
        self.entries.push(RecordingLogEntry {
            date: Local::now().date_naive(),
            platform,
            duration_secs,
        });
        // Keep the log from growing unbounded - 365 days of history is plenty.
        if self.entries.len() > 5000 {
            self.entries.drain(0..(self.entries.len() - 5000));
        }
        self.save()
    }

    /// Returns (count, total_duration_secs) for today.
    pub fn today_summary(&self) -> (usize, i64) {
        let today = Local::now().date_naive();
        let todays: Vec<&RecordingLogEntry> = self.entries.iter().filter(|e| e.date == today).collect();
        let total: i64 = todays.iter().map(|e| e.duration_secs).sum();
        (todays.len(), total)
    }
}
