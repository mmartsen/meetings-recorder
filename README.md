# meeting-recorder

A Linux tray app that watches your system audio, automatically detects when you're in a real
meeting (Discord, Google Meet, MS Teams, Slack huddles, Zoom, ...), and records it — with an
optional Google Calendar cross-check and a fully pluggable post-processing pipeline (Whisper,
diarization, upload, whatever you want) that runs as external commands you configure yourself.

## How it works

- **Audio detection**: reads live audio-routing info from PipeWire/PulseAudio via `pactl`
  (`pactl subscribe` for instant change events, `pactl list source-outputs|sink-inputs` for
  who's using the mic / producing sound). This is how it knows the mic just went active and
  which app is talking.
- **Platform detection**: matches the app name/binary against a configurable list
  (Discord, Teams, Slack, Zoom, Skype). For browser-based calls (Google Meet, Slack huddles,
  Teams web) it additionally checks the active window title via `wmctrl` for things like
  `meet.google.com`.
- **Meeting vs. noise**: a confidence score (0-100) built from independent signals — mic active,
  known app producing audio, browser meeting-tab title, sustained duration — so a 2-second
  notification ping doesn't trigger a recording. Tunable via `confidence_threshold` and
  `min_duration_secs` in the config.
- **Recording**: `ffmpeg` capturing the default mic input and/or `@DEFAULT_SINK@.monitor`
  (everything currently playing), encoded to Opus (or MP3) in your recordings folder.
- **Calendar cross-check (optional)**: see "Calendar backends" below.
- **Pipeline hooks (optional)**: after a recording finishes, each configured hook is run as
  `<command> <args...> <audio_file> <metadata.json>`. This is intentionally *not* baked into the
  Rust code — wire in Whisper, a diarizer, a summarizer, an auto-uploader, or your own script,
  entirely outside this codebase.
- **Tray icon**: shows recording status live, lets you force start/stop (overriding automatic
  detection until toggled again), shows today's recording count/duration, and gives quick access
  to the config file and recordings folder.
- **Startup system-info log**: on every launch, logs your session type (Wayland/X11), desktop
  environment (`XDG_CURRENT_DESKTOP`), and whether each external tool it depends on (`pactl`,
  `ffmpeg`, `wmctrl`, `curl`) is actually present on `PATH` — so a missing dependency shows up
  immediately in the logs instead of as a silent failure later. It also notes when running under
  Wayland that `wmctrl`-based browser-tab detection typically only sees XWayland windows (native
  Wayland apps aren't affected — that detection path is independent, via `pactl`).

## Calendar backends

Set `calendar.provider` in the config to one of:

- **`"none"`** (default) — no calendar cross-check.
- **`"ics"`** — reads local `.ics` calendar files directly. No OAuth, no network, no Google
  account required. This is the recommended option if you use **KDE's calendar (KOrganizer)**:
  with `ics.auto_discover_kde = true` (the default), the app scans `~/.config` for Akonadi's
  local ICal-resource config files and automatically reads whatever `.ics` file(s) they point
  at — zero extra setup if you already have a local ICS calendar added in KOrganizer. You can
  also point it at specific files/directories via `ics.paths` (works for Thunderbird calendars,
  vdirsyncer/davical-synced folders, or any plain `.ics` export).

  **Important limitation**: this only sees *local* ICS resources. If your KDE calendar syncs a
  Google/CalDAV/Exchange account through Akonadi's own sync agents, those events live in
  Akonadi's internal database, not a flat file — Akonadi doesn't expose event contents over
  simple D-Bus calls, and reading its internal store properly would require linking against KDE
  Frameworks/Qt, which this lightweight Rust daemon deliberately avoids. Two ways around that:
  add a second, separate local ICS calendar in KOrganizer for this tool to read, or use the
  `google` provider below to talk to the same account directly.

- **`"google"`** — talks to the Google Calendar API directly via OAuth (see setup below). Use
  this if you want to cross-check a Google account that isn't (or can't be) mirrored as a local
  `.ics` file.

### Google Calendar setup (one-time, optional)

1. Create a Google Cloud project, enable the **Google Calendar API**.
2. Create an OAuth 2.0 Client ID of type "Desktop app" — this gives you `client_id` and
   `client_secret`.
3. Use the OAuth 2.0 Playground (or any installed-app OAuth flow) to authorize the
   `https://www.googleapis.com/auth/calendar.readonly` scope with that client, and exchange the
   authorization code for a **refresh token**.
4. Put all three values into `config.toml` under `[calendar.google]` and set
   `calendar.provider = "google"`.

The app only ever calls the read-only `events.list` endpoint on your primary calendar, to look
for an event whose time window currently overlaps "now".

## Build

Requires Rust (install via [rustup](https://rustup.rs), not your distro's apt package — this
project targets current Cargo/edition-2021 dependency versions).

```bash
cargo build --release
# binary at target/release/meeting-recorder
```

System dependencies (install via your package manager):
- `pulseaudio-utils` or `pipewire-pulse` (provides `pactl`)
- `ffmpeg`
- `wmctrl` (optional, improves browser-based meeting detection)
- `curl` (only needed if Google Calendar cross-check is enabled)
- A StatusNotifierItem tray host — GNOME (with an extension like "AppIndicator and KStatusNotifierItem
  Support"), KDE Plasma, or most other DEs. Tray icons are a DE-level feature outside this app's
  control; if your tray doesn't show it, that's the compositor/DE, not this binary.

## Running

```bash
./target/release/meeting-recorder
```

On first run it writes a default config to `~/.config/meeting-recorder/config.toml` and starts
watching. Recordings land in `~/Recordings/meetings/` by default.

To run it automatically at login as a background service, see `packaging/meeting-recorder.service`
(a user-level systemd unit) — copy it to `~/.config/systemd/user/` and:

```bash
systemctl --user daemon-reload
systemctl --user enable --now meeting-recorder.service
```

## Configuration

Edit `~/.config/meeting-recorder/config.toml`. Key sections:

- `[detection]` — add/remove known apps (`known_apps`), browser meeting-title matches
  (`browser_meeting_titles`), confidence threshold, minimum duration.
- `[recording]` — output directory, capture mode (`mic` / `call_audio` / `both`), format
  (`opus` / `mp3`).
- `[pipeline]` — set `enabled = true` and add hooks. Example Whisper hook:

  ```toml
  [pipeline]
  enabled = true

  [[pipeline.hooks]]
  name = "whisper-transcribe"
  command = "/usr/bin/whisper"
  args = ["--model", "base", "--output_format", "txt", "--output_dir", "/home/you/Recordings/meetings"]
  required = false
  ```

  Whisper (or any hook) receives the recording's audio file path and its metadata JSON
  (platform, start/end time, confidence, matched calendar event if any) as its last two
  arguments, so you can build arbitrarily complex downstream pipelines (transcription →
  summarization → Notion upload, etc.) without touching this codebase.

- `[calendar]` — set `provider = "ics"` (for KDE/local calendars) or `"google"`, plus the
  matching `[calendar.ics]` or `[calendar.google]` sub-table. See "Calendar backends" above.

## Notes on consent

Recording calls can have legal implications depending on your jurisdiction (some places require
all-party consent) and may violate the ToS of some platforms if you record others without
telling them. This tool records your own machine's audio; how and when you use those recordings
is on you.

## Project layout

```
src/
  main.rs        - wires everything together: detection loop, recorder lifecycle, tray thread
  system_info.rs - logs session type, desktop environment, and tool availability at startup
  audio.rs       - pactl-based mic/app-audio snapshot + subscribe watcher
  window.rs      - active window titles (wmctrl) for browser-based meeting detection
  detector.rs    - platform + meeting-confidence scoring
  recorder.rs    - ffmpeg process lifecycle (start/stop)
  pipeline.rs    - metadata file + external hook runner
  calendar/
    mod.rs       - dispatches to whichever provider is configured
    google.rs    - Google Calendar cross-check (via curl, no heavy HTTP deps)
    ics.rs       - local .ics file cross-check, with Akonadi/KDE auto-discovery
  stats.rs       - daily recording stats, persisted to ~/.local/share/meeting-recorder/stats.json
  state.rs       - shared app state (Arc<Mutex<..>>) used by both the tray and main loop
  tray.rs        - ksni system tray icon/menu
  config.rs      - TOML config schema + defaults
packaging/
  meeting-recorder.service - example systemd --user unit
```
