mod audio;
mod calendar;
mod config;
mod detector;
mod icons;
mod logging;
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
    let _log_guard = logging::init();
    tracing::info!(
        "meeting-recorder {} starting (pid {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );

    system_info::log_system_info();
    icons::install();

    let cfg = Config::load_or_create()?;
    tracing::info!("loaded config, output dir = {:?}", cfg.recording.output_dir);
    tracing::info!(
        "detection: threshold={} min_duration={}s poll={}s known_apps=[{}] browsers=[{}] calendar={}",
        cfg.detection.confidence_threshold,
        cfg.detection.min_duration_secs,
        cfg.general.poll_interval_secs,
        cfg.detection
            .known_apps
            .iter()
            .map(|a| format!("{}={}", a.process_match, a.platform))
            .collect::<Vec<_>>()
            .join(", "),
        cfg.detection.browser_processes.join(", "),
        cfg.calendar.provider,
    );

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
            Ok(handle) => {
                tracing::info!("tray icon service started");
                handle
            }
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

    // Audio change events, pushed by `pactl subscribe` (restarted automatically if it dies).
    let (tx, mut rx) = mpsc::channel(16);
    tokio::spawn(audio::watch_forever(tx));

    let mut active: Option<ActiveRecording> = None;
    let mut meeting_since: Option<std::time::Instant> = None;
    let mut poll = tokio::time::interval(Duration::from_secs(cfg.general.poll_interval_secs));
    let mut events_open = true;

    // Previous-iteration values, used to log only on *changes* at info level so the log stays
    // readable over days while still recording every decision that matters.
    let mut last_snapshot: Option<audio::AudioSnapshot> = None;
    let mut last_decision: Option<(Option<String>, u8)> = None;
    let mut last_force: Option<bool> = None;
    let mut suppressed_logged = false;
    let mut snapshot_failing = false;

    // systemd stop/restart, logout and the tray's Quit all arrive as SIGTERM/SIGINT. Finish an
    // active recording properly (metadata, hooks, stats) instead of dying mid-file.
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    loop {
        let trigger = tokio::select! {
            _ = sigterm.recv() => "SIGTERM",
            _ = sigint.recv() => "SIGINT",
            ev = rx.recv(), if events_open => {
                if ev.is_none() {
                    // Shouldn't happen (watch_forever loops), but never spin on a closed channel.
                    tracing::error!("audio event channel closed; falling back to polling only");
                    events_open = false;
                }
                "pactl-event"
            }
            _ = poll.tick() => "poll",
        };

        if trigger == "SIGTERM" || trigger == "SIGINT" {
            tracing::info!("received {trigger}, shutting down");
            if let Some(rec) = active.take() {
                tracing::info!("finalizing active recording before exit");
                finish_recording(&cfg, &shared, rec).await;
            }
            tracing::info!("shutdown complete");
            return Ok(());
        }

        let force = shared.lock().unwrap().force_override;
        if force != last_force {
            tracing::info!("force override changed: {:?} -> {:?}", last_force, force);
            last_force = force;
            suppressed_logged = false;
        }

        // Detect ffmpeg dying underneath us; otherwise we'd sit "recording" nothing forever.
        if let Some(rec) = active.as_mut() {
            if let Some(status) = recorder::exited_early(rec) {
                tracing::error!(
                    "ffmpeg exited unexpectedly ({status}) while recording {:?}; finalizing",
                    rec.output_path
                );
                let rec = active.take().unwrap();
                finish_recording(&cfg, &shared, rec).await;
                meeting_since = None;
            }
        }

        let snapshot = match tokio::task::spawn_blocking(audio::snapshot).await {
            Ok(Ok(s)) => {
                if snapshot_failing {
                    tracing::info!("audio snapshot recovered");
                    snapshot_failing = false;
                }
                s
            }
            Ok(Err(e)) => {
                // Log the first failure at warn, then only at debug until it recovers.
                if !snapshot_failing {
                    tracing::warn!("failed to read audio snapshot: {e:#}");
                    snapshot_failing = true;
                } else {
                    tracing::debug!("audio snapshot still failing: {e:#}");
                }
                continue;
            }
            Err(e) => {
                tracing::error!("audio snapshot task panicked: {e}");
                continue;
            }
        };
        if last_snapshot.as_ref() != Some(&snapshot) {
            tracing::debug!("audio streams changed ({trigger}): {snapshot}");
            last_snapshot = Some(snapshot.clone());
        }

        let detection = detector::detect(&snapshot, &cfg.detection);
        let above = detection.confidence >= cfg.detection.confidence_threshold;
        tracing::trace!(
            "tick ({trigger}): confidence={} platform={:?} reasons=[{}]",
            detection.confidence,
            detection.platform,
            detection.reasons.join("; ")
        );

        let decision = (detection.platform.clone(), detection.confidence);
        if last_decision.as_ref() != Some(&decision) {
            tracing::info!(
                "detection changed: platform={:?} confidence={} (threshold {}) -> {} | reasons: [{}] | {}",
                detection.platform,
                detection.confidence,
                cfg.detection.confidence_threshold,
                if above { "MEETING" } else { "no meeting" },
                detection.reasons.join("; "),
                snapshot
            );
            last_decision = Some(decision);
        }

        let should_record = match force {
            Some(v) => {
                if !v && above && active.is_none() && !suppressed_logged {
                    tracing::warn!(
                        "meeting detected ({:?}, confidence {}) but auto-recording is suppressed by a force-stop override from the tray",
                        detection.platform,
                        detection.confidence
                    );
                    suppressed_logged = true;
                }
                v
            }
            None => {
                if above {
                    // Require sustained signal for min_duration_secs before auto-starting,
                    // so a 2-second notification ping doesn't trigger a recording.
                    let since = meeting_since.get_or_insert_with(|| {
                        if active.is_none() {
                            tracing::info!(
                                "meeting candidate: waiting {}s of sustained signal before recording",
                                cfg.detection.min_duration_secs
                            );
                        }
                        std::time::Instant::now()
                    });
                    since.elapsed().as_secs() >= cfg.detection.min_duration_secs
                } else {
                    if let Some(since) = meeting_since.take() {
                        if active.is_none() {
                            tracing::info!(
                                "meeting candidate dropped after {}s (needed {}s)",
                                since.elapsed().as_secs(),
                                cfg.detection.min_duration_secs
                            );
                        }
                    }
                    false
                }
            }
        };

        if should_record && active.is_none() {
            tracing::info!(
                "starting recording, detected platform = {:?} (confidence {}, force={:?}, reasons: {})",
                detection.platform,
                detection.confidence,
                force,
                detection.reasons.join("; ")
            );
            match recorder::start(
                &cfg.recording,
                detection.platform.clone(),
                detection.confidence,
            )
            .await
            {
                Ok(rec) => {
                    {
                        let mut st = shared.lock().unwrap();
                        st.recording = true;
                        st.current_platform = detection.platform.clone();
                    }
                    active = Some(rec);
                    if cfg.general.notify_on_start_stop {
                        notify(
                            "Meeting recording started",
                            &format!(
                                "Platform: {}",
                                detection.platform.as_deref().unwrap_or("unknown")
                            ),
                        )
                        .await;
                    }
                }
                Err(e) => tracing::error!("failed to start recording: {e:#}"),
            }
        } else if !should_record && active.is_some() {
            tracing::info!(
                "stopping recording (confidence {}, force={:?}, reasons: {})",
                detection.confidence,
                force,
                detection.reasons.join("; ")
            );
            let rec = active.take().unwrap();
            finish_recording(&cfg, &shared, rec).await;
            meeting_since = None;
        }
    }
}

/// Stops ffmpeg, writes metadata, runs pipeline hooks, updates stats and notifies.
async fn finish_recording(cfg: &Config, shared: &SharedState, rec: ActiveRecording) {
    let started_at = rec.started_at;
    let confidence = rec.confidence;
    let platform = rec.platform.clone();
    let result = recorder::stop(rec).await;

    {
        let mut st = shared.lock().unwrap();
        st.recording = false;
        st.current_platform = None;
    }

    let path = match result {
        Ok(path) => path,
        Err(e) => {
            tracing::error!("failed to stop recording cleanly: {e:#}");
            return;
        }
    };

    let ended_at = chrono::Local::now();
    let duration = (ended_at - started_at).num_seconds();
    tracing::info!(
        "recording finished: {:?}, {}s, platform {:?}",
        path,
        duration,
        platform
    );

    let calendar_title = if cfg.calendar.provider != "none" {
        match calendar::current_event_title(&cfg.calendar).await {
            Ok(t) => {
                tracing::info!("calendar cross-check ({}): {:?}", cfg.calendar.provider, t);
                t
            }
            Err(e) => {
                tracing::warn!(
                    "calendar cross-check ({}) failed: {e:#}",
                    cfg.calendar.provider
                );
                None
            }
        }
    } else {
        None
    };

    let meta = pipeline::RecordingMetadata {
        started_at,
        ended_at,
        duration_secs: duration,
        platform: platform.clone(),
        confidence,
        calendar_event_title: calendar_title,
        audio_file: path.clone(),
    };
    match pipeline::write_metadata(&path, &meta) {
        Ok(meta_path) if cfg.pipeline.enabled => {
            // Run hooks in the background: transcription can take longer than the meeting,
            // and the detection loop must keep watching for the next call meanwhile.
            let pipeline_cfg = cfg.pipeline.clone();
            let audio = path.clone();
            tokio::spawn(async move {
                pipeline::run_hooks(&pipeline_cfg, &audio, &meta_path).await;
            });
        }
        Ok(_) => {}
        Err(e) => tracing::error!("failed to write metadata for {:?}: {e:#}", path),
    }

    if let Err(e) = shared
        .lock()
        .unwrap()
        .stats
        .record(platform.clone(), duration)
    {
        tracing::warn!("failed to update stats: {e:#}");
    }

    if cfg.general.notify_on_start_stop {
        notify(
            "Meeting recording saved",
            &format!(
                "{} ({}s) -> {:?}",
                platform.as_deref().unwrap_or("unknown"),
                duration,
                path.file_name().unwrap_or_default()
            ),
        )
        .await;
    }
}

/// Must match the installed `.desktop` file's basename (packaging/meeting-recorder.desktop).
/// Plasma only keeps notifications in its history (and lists the app in per-app notification
/// settings) when it can resolve the sender to a desktop entry.
const DESKTOP_ENTRY: &str = "meeting-recorder";

/// Desktop notification via the async API. The blocking `.show()` must NOT be used here:
/// ksni pulls in zbus's tokio integration, so notify-rust's blocking call does a `block_on`
/// inside the tokio runtime and panics ("Cannot start a runtime from within a runtime").
async fn notify(summary: &str, body: &str) {
    if let Err(e) = notify_rust::Notification::new()
        .appname("Meeting Recorder")
        .icon(icons::IDLE)
        .hint(notify_rust::Hint::DesktopEntry(DESKTOP_ENTRY.into()))
        .summary(summary)
        .body(body)
        .show_async()
        .await
    {
        tracing::warn!("desktop notification failed: {e}");
    }
}
