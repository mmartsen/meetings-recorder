<p align="center">
  <img src="assets/icons/meeting-recorder.svg" alt="Meeting Recorder icon" width="96" height="96">
</p>

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
  Teams web) it matches the tab title against `browser_meeting_titles`. The title comes from
  the browser's own audio stream (`media.name`, e.g. `Meet - Weekly sync`), which works on
  Wayland too. Window titles from `wmctrl` are also checked, but on Wayland it only sees
  XWayland windows.
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
  entirely outside this codebase. A ready-made Whisper transcription hook is included (see
  [Transcription with Whisper](#transcription-with-whisper)).
- **Tray icon**: a blue speech bubble when idle, red while recording. The menu shows today's
  recording count and duration, lets you force start/stop, and opens the config file or
  recordings folder. Note that force start/stop pins that state and turns off automatic
  detection until you click it again (or restart the app).
- **Notifications**: a desktop notification when a recording starts and when it's saved
  (`general.notify_on_start_stop`). With the `.desktop` file installed, Plasma keeps them in
  its notification history and lists "Meeting Recorder" in its per-app notification settings.
- **Clean shutdown**: stopping or restarting the service, logging out, or Quit from the tray
  finishes an active recording properly (file finalized, metadata written, hooks run) before
  exiting.
- **Startup system-info log**: on every launch, logs your session type (Wayland/X11), desktop
  environment (`XDG_CURRENT_DESKTOP`), and whether each external tool it depends on (`pactl`,
  `ffmpeg`, `wmctrl`, `curl`) is actually present on `PATH` — so a missing dependency shows up
  immediately in the logs instead of as a silent failure later.

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

## Install

### 1. System dependencies

- `pactl`: from `pipewire-pulse` (or `pulseaudio-utils` on PulseAudio systems). Required.
- `ffmpeg`, built with PulseAudio input support (the default on Arch/Manjaro, Fedora, Debian,
  Ubuntu). Required.
- `wmctrl`: optional; window-title matching on X11. Browser meetings are detected without it.
- `curl`: only if you use the Google Calendar cross-check.
- A StatusNotifierItem tray host. KDE Plasma and most desktops have one built in; stock GNOME
  needs the "AppIndicator and KStatusNotifierItem Support" extension.

To build you also need Rust from [rustup](https://rustup.rs). Distro packages are often too
old. No system libraries are needed to build; all dependencies are pure Rust.

```bash
# Arch / Manjaro
sudo pacman -S --needed pipewire-pulse ffmpeg wmctrl
# Debian / Ubuntu
sudo apt install pipewire-pulse ffmpeg wmctrl
# Fedora (ffmpeg from RPM Fusion, or the stock ffmpeg-free package)
sudo dnf install pipewire-pulseaudio ffmpeg-free wmctrl
```

### 2. Build and install

```bash
./install.sh
```

This builds a release binary and installs, for your user only (no root):

| What | Where |
|---|---|
| binary | `~/.local/bin/meeting-recorder` |
| desktop entry | `~/.local/share/applications/meeting-recorder.desktop` |
| systemd user service (enabled + started) | `~/.config/systemd/user/meeting-recorder.service` |
| pipeline hook scripts | `~/.local/share/meeting-recorder/hooks/` (e.g. `whisper-transcribe.sh`) |

The app installs its own icons into `~/.local/share/icons/hicolor/scalable/apps/` on startup.
Change the binary location with `BIN_DIR=~/bin ./install.sh`. Other flags:

- `--no-service`: skip the systemd unit, if you'd rather start it some other way
  (e.g. KDE Autostart).
- `--no-build`: install an existing `target/release/meeting-recorder` without rebuilding.
- `--uninstall`: remove everything above. Your config, recordings, stats and logs are kept.

**Updating**: `git pull && ./install.sh`. Restarting during a call is safe: the current
recording is finished and saved first, and a new one starts once the call is detected again.
The call still ends up split across two files.

### Manual install

```bash
cargo build --release
install -Dm755 target/release/meeting-recorder ~/.local/bin/meeting-recorder
cp packaging/meeting-recorder.desktop ~/.local/share/applications/   # set Exec= to the binary path
cp packaging/meeting-recorder.service ~/.config/systemd/user/        # set ExecStart= if not ~/.local/bin
systemctl --user daemon-reload
systemctl --user enable --now meeting-recorder.service
```

Install the desktop entry even if you never launch it from the menu. Plasma only keeps an
app's notifications in its history when it can match them to an installed `.desktop` file.

## Running

As a service (after `./install.sh`):

```bash
systemctl --user status meeting-recorder
systemctl --user restart meeting-recorder
journalctl --user -u meeting-recorder -f
```

Or run the binary in a terminal: `~/.local/bin/meeting-recorder` (stop the service first, or
two copies will both record).

On first run it writes a default config to `~/.config/meeting-recorder/config.toml` and starts
watching. Recordings land in `~/Recordings/meetings/` by default, each with a `.json` metadata
file next to it.

## Logs

Logs go to two places:

- **stderr** (so `journalctl --user -u meeting-recorder -f` when running as a service), at the
  level set by `RUST_LOG` (default `info`).
- **a daily-rotated file** in `~/.local/state/meeting-recorder/logs/meeting-recorder.YYYY-MM-DD.log`
  (14 days kept), at `info,meeting_recorder=debug` by default. You can override this with
  `MEETING_RECORDER_FILE_LOG`, using the same syntax as `RUST_LOG`.

At `info`, every detection state change is logged with its confidence, its reasons and the
current mic/audio streams. That covers meeting candidates, candidates that drop out before
`min_duration_secs`, start/stop, ffmpeg pid/exit status/output size, and force-override
changes. So "why didn't this call get recorded?" can usually be answered from one log. At
`debug` you also get every audio-stream change. At `trace` you get every tick and raw
`pactl subscribe` event. Panics are written to the file log with a backtrace.

## Configuration

Edit `~/.config/meeting-recorder/config.toml`. Key sections:

- `[detection]` — add/remove known apps (`known_apps`), browser meeting-title matches
  (`browser_meeting_titles`), confidence threshold, minimum duration.
- `[recording]` — output directory, capture mode (`mic` / `call_audio` / `both`), format
  (`opus` / `mp3`).
- `[pipeline]` — commands to run after each recording, e.g. transcription. See
  [Post-processing pipeline](#post-processing-pipeline) and
  [Transcription with Whisper](#transcription-with-whisper) below.

- `[calendar]` — set `provider = "ics"` (for KDE/local calendars) or `"google"`, plus the
  matching `[calendar.ics]` or `[calendar.google]` sub-table. See "Calendar backends" above.

## Post-processing pipeline

After each recording is saved, every hook in `[pipeline]` runs in order as:

```
<command> <args...> <audio_file> <metadata_json>
```

- `metadata_json` is the `.json` file next to the recording: start/end time, duration,
  platform, detection confidence, and the matched calendar event title if any.
- Hooks run in the background, so a long transcription doesn't stop the next meeting from
  being detected and recorded.
- A hook that fails or can't be started is logged with its stderr. If it has
  `required = true`, the hooks after it are skipped for that recording.
- Each hook's start, duration and exit status are in the log (`grep hook` in the log file).
- Hooks are killed if the app stops mid-run (service restart, logout). Re-run them by hand
  on the recording if needed.
- Because hooks run in order, later hooks can use earlier hooks' output (e.g. a summarizer
  that reads the `.txt` transcript).

## Transcription with Whisper

`install.sh` installs a ready-made hook,
`~/.local/share/meeting-recorder/hooks/whisper-transcribe.sh`
([source](examples/hooks/whisper-transcribe.sh)). It writes the transcript next to the
recording:

```
2026-11-24_12-49-51_Google-Meet.opus
2026-11-24_12-49-51_Google-Meet.opus.json
2026-11-24_12-49-51_Google-Meet.txt     <- plain transcript
2026-11-24_12-49-51_Google-Meet.srt     <- with timestamps
```

It works with whichever of these three Whisper implementations you have installed.

### 1. Install a Whisper implementation

Pick one:

| Implementation | Good for | Install |
|---|---|---|
| **[whisper.cpp](https://github.com/ggml-org/whisper.cpp)** (recommended) | Fast on CPU, no Python. GPU builds (CUDA, Vulkan) available. | AUR `whisper.cpp`, or build from source (below) |
| **[faster-whisper](https://github.com/Softcatala/whisper-ctranslate2)** | Fast, Python-based, NVIDIA GPU via CUDA | `pipx install whisper-ctranslate2` |
| **[openai-whisper](https://github.com/openai/whisper)** | The reference implementation. Slowest on CPU, large PyTorch install | `pipx install openai-whisper` |

**whisper.cpp from source** (any distro; needs `git`, `cmake` and a C++ compiler):

```bash
git clone https://github.com/ggml-org/whisper.cpp
cd whisper.cpp
cmake -B build && cmake --build build -j --config Release
install -m755 build/bin/whisper-cli ~/.local/bin/
```

whisper.cpp also needs a model file, which the hook looks for in
`~/.local/share/whisper.cpp/models/` (override with `WHISPER_CPP_MODELS`, or pass a full path
as `--model`):

```bash
mkdir -p ~/.local/share/whisper.cpp/models
curl -L -o ~/.local/share/whisper.cpp/models/ggml-base.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin
```

faster-whisper and openai-whisper download models automatically the first time they're used.

The hook finds the binary on `PATH`. The systemd service's `PATH` may not include
`~/.local/bin`, so if the log says `no Whisper found on PATH`, add
`Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin` to
`~/.config/systemd/user/meeting-recorder.service` and run `systemctl --user daemon-reload`.

### 2. Enable the hook

In `~/.config/meeting-recorder/config.toml`:

```toml
[pipeline]
enabled = true

[[pipeline.hooks]]
name = "whisper-transcribe"
command = "/home/you/.local/share/meeting-recorder/hooks/whisper-transcribe.sh"
args = ["--model", "small", "--language", "en"]
required = false
```

`command` must be an absolute path (no `~`). Then `systemctl --user restart meeting-recorder`.
Config files created by a recent version already contain this hook, disabled; just set
`enabled = true`.

Hook options (in `args`):

| Option | Default | Meaning |
|---|---|---|
| `--backend` | `auto` | `whisper.cpp`, `faster-whisper` or `openai`; `auto` uses the first one installed, in that order |
| `--model` | `base` | `tiny`, `base`, `small`, `medium`, `large-v3`, `large-v3-turbo`; `.en` variants (e.g. `small.en`) are faster and more accurate for English-only calls |
| `--language` | `auto` | spoken language code (`en`, `uk`, `de`...). Setting it avoids wrong auto-detection on calls that start quietly or in another language |
| `--threads` | all cores | CPU threads |
| `--formats` | `txt,srt` | any of `txt,srt,vtt,json` |
| `--min-seconds` | `10` | skip recordings shorter than this |

Choosing a model: `base` is fast and fine for clear English. `small` is a good default for
meetings. `medium` and `large-v3-turbo` are noticeably better with accents, crosstalk and
non-English speech, but much slower on CPU; use them if you have a GPU. Only one
transcription runs at a time; if two calls end close together, the second waits.

### 3. Test it

Run the hook by hand on an existing recording. It's the same command the app runs:

```bash
~/.local/share/meeting-recorder/hooks/whisper-transcribe.sh --model small \
  ~/Recordings/meetings/<recording>.opus ~/Recordings/meetings/<recording>.opus.json
```

After the next real call, check the log for
`running pipeline hook 'whisper-transcribe'` and `hook 'whisper-transcribe' completed in Ns`.
If it failed, the log line includes the script's error message (missing model, Whisper not
on `PATH`, etc.).

## Troubleshooting

- **A call wasn't recorded.** Check the log for that time:
  `grep -E "detection changed|candidate|starting|stopping|WARN|ERROR" ~/.local/state/meeting-recorder/logs/meeting-recorder.$(date +%F).log`.
  Each `detection changed` line shows the confidence score, why it was given, and which
  streams were active. A browser call stuck around 35 means the tab title didn't match any
  `browser_meeting_titles` entry; add a substring of the tab title (shown in the line as
  `Firefox[firefox]{"..."}`). Also check for a `suppressed by a force-stop override` warning.
- **No tray icon.** On GNOME, install the AppIndicator extension. Otherwise run the binary in
  a terminal and look for `tray icon service started` or a tray error.
- **Notifications aren't kept in history / no app icon on them.** Install the desktop entry
  (`./install.sh` does it) and log out and back in, or run `kbuildsycoca6` on KDE.
- **Empty or very short files.** Look for `ffmpeg exited ... wrote N bytes` and `ffmpeg:`
  warnings in the log. Make sure `ffmpeg -f pulse -i default -t 3 /tmp/t.opus` works.

## Notes on consent

Recording calls can have legal implications depending on your jurisdiction (some places require
all-party consent) and may violate the ToS of some platforms if you record others without
telling them. This tool records your own machine's audio; how and when you use those recordings
is on you.

## Project layout

```
src/
  main.rs        - wires everything together: detection loop, recorder lifecycle, tray thread
  logging.rs     - tracing setup: stderr + rotating file log, panic hook
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
  icons.rs       - bundled icons: tray pixmaps, SVGs installed to ~/.local/share/icons
  config.rs      - TOML config schema + defaults
assets/icons/    - idle / recording icons: SVG sources + PNG renders for the tray
packaging/
  meeting-recorder.service - systemd --user unit (installed by install.sh)
  meeting-recorder.desktop - desktop entry (installed by install.sh)
install.sh       - build + per-user install / update / uninstall
examples/hooks/
  whisper-transcribe.sh    - Whisper transcription hook (whisper.cpp / faster-whisper / openai)
```

The tray PNGs are rendered from the SVGs. After editing an SVG, regenerate them:

```bash
for n in meeting-recorder meeting-recorder-recording; do for sz in 22 32 48 64; do
  rsvg-convert -w $sz -h $sz assets/icons/$n.svg -o assets/icons/png/$n-$sz.png; done; done
```

## License

Released into the public domain under [the Unlicense](LICENSE). See
[CONTRIBUTING.md](CONTRIBUTING.md) for how to report issues.
