mod audio;
mod calendar;
mod config;
mod detector;
mod pipeline;
mod recorder;
mod state;
mod stats;
mod system_info;
mod tray;
mod window;

use config::Config;
use recorder::ActiveRecording;
use state::{AppState, SharedState};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    system_info::log_system_info();

    let cfg = Config::load_or_create()?;
    tracing::info!("loaded config, output dir = {:?}", cfg.recording.output_dir);

    let shared: SharedState = Arc::new(Mutex::new(AppState::new(cfg.recording.output_dir.clone())));

    // Tray runs as a tokio task via ksni's async D-Bus service. Recording keeps working even
    // if no StatusNotifierItem host is available.
    let tray_state = shared.clone();
    tokio::spawn(async move {
        use ksni::TrayMethods;
        // assume_sni_available: when started as a systemd --user service, the tray host may
        // not be up yet; keep waiting for it instead of failing immediately.
        let tray = tray::RecorderTray { state: tray_state };
        let handle = match tray.assume_sni_available(true).spawn().await {
            Ok(handle) => handle,
            Err(e) => {
                tracing::error!("failed to start tray icon: {e}");
                return;
            }
        };
        // The tray reads SharedState lazily; poke it periodically so the icon/tooltip reflect
        // recording state changes made by the main loop.
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            if handle.update(|_| {}).await.is_none() {
                tracing::warn!("tray service shut down");
                break;
            }
        }
    });

    // Audio change events, pushed by `pactl subscribe`.
    let (tx, mut rx) = mpsc::channel(16);
    let _watch_handle = tokio::spawn(async move {
        if let Err(e) = audio::watch(tx).await {
            tracing::error!("audio watcher exited: {e:#}");
        }
    });

    let mut active: Option<ActiveRecording> = None;
    let mut meeting_since: Option<std::time::Instant> = None;
    let mut poll = tokio::time::interval(Duration::from_secs(cfg.general.poll_interval_secs));

    loop {
        tokio::select! {
            _ = rx.recv() => {}
            _ = poll.tick() => {}
        }

        let force = shared.lock().unwrap().force_override;

        let snapshot = match audio::snapshot() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to read audio snapshot: {e:#}");
                continue;
            }
        };
        let detection = detector::detect(&snapshot, &cfg.detection);

        let should_record = match force {
            Some(v) => v,
            None => {
                if detection.confidence >= cfg.detection.confidence_threshold {
                    // Require sustained signal for min_duration_secs before auto-starting,
                    // so a 2-second notification ping doesn't trigger a recording.
                    let since = meeting_since.get_or_insert_with(std::time::Instant::now);
                    since.elapsed().as_secs() >= cfg.detection.min_duration_secs
                } else {
                    meeting_since = None;
                    false
                }
            }
        };

        if should_record && active.is_none() {
            tracing::info!(
                "starting recording, detected platform = {:?} (confidence {}, reasons: {})",
                detection.platform,
                detection.confidence,
                detection.reasons.join("; ")
            );
            match recorder::start(&cfg.recording, detection.platform.clone()).await {
                Ok(rec) => {
                    if cfg.general.notify_on_start_stop {
                        let _ = notify_rust::Notification::new()
                            .summary("Meeting recording started")
                            .body(&format!(
                                "Platform: {}",
                                detection
                                    .platform
                                    .clone()
                                    .unwrap_or_else(|| "unknown".into())
                            ))
                            .show();
                    }
                    let mut st = shared.lock().unwrap();
                    st.recording = true;
                    st.current_platform = detection.platform.clone();
                    active = Some(rec);
                }
                Err(e) => tracing::error!("failed to start recording: {e:#}"),
            }
        } else if !should_record && active.is_some() {
            let rec = active.take().unwrap();
            let started_at = rec.started_at;
            let platform = rec.platform.clone();
            tracing::info!("stopping recording");
            match recorder::stop(rec).await {
                Ok(path) => {
                    let ended_at = chrono::Local::now();
                    let duration = (ended_at - started_at).num_seconds();

                    let calendar_title = if cfg.calendar.provider != "none" {
                        calendar::current_event_title(&cfg.calendar)
                            .await
                            .unwrap_or(None)
                    } else {
                        None
                    };

                    let meta = pipeline::RecordingMetadata {
                        started_at,
                        ended_at,
                        duration_secs: duration,
                        platform: platform.clone(),
                        confidence: detection.confidence,
                        calendar_event_title: calendar_title,
                        audio_file: path.clone(),
                    };
                    if let Ok(meta_path) = pipeline::write_metadata(&path, &meta) {
                        pipeline::run_hooks(&cfg.pipeline, &path, &meta_path).await;
                    }

                    {
                        let mut st = shared.lock().unwrap();
                        st.recording = false;
                        st.current_platform = None;
                        let _ = st.stats.record(platform.clone(), duration);
                    }

                    if cfg.general.notify_on_start_stop {
                        let _ = notify_rust::Notification::new()
                            .summary("Meeting recording saved")
                            .body(&format!(
                                "{} ({}s) -> {:?}",
                                platform.unwrap_or_else(|| "unknown".into()),
                                duration,
                                path.file_name().unwrap_or_default()
                            ))
                            .show();
                    }
                }
                Err(e) => tracing::error!("failed to stop recording cleanly: {e:#}"),
            }
            meeting_since = None;
        }
    }
}
