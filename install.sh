#!/usr/bin/env bash
# Builds and installs meeting-recorder for the current user (no root needed):
#   - binary        -> $BIN_DIR/meeting-recorder           (default ~/.local/bin)
#   - desktop entry -> $APP_DIR/meeting-recorder.desktop   (default ~/.local/share/applications)
#   - systemd unit  -> $UNIT_DIR/meeting-recorder.service  (default ~/.config/systemd/user)
#   - hook scripts  -> ~/.local/share/meeting-recorder/hooks/ (whisper / cloud transcription)
# then enables and (re)starts the service. Re-run it to update: restarting is safe, an active
# recording is finalized before the old process exits.
#
# Usage: ./install.sh [--no-build] [--no-service] [--uninstall]
#   --no-build    install the existing target/release/meeting-recorder without rebuilding
#   --no-service  don't install/enable the systemd user unit (e.g. you autostart it yourself)
#   --uninstall   stop/disable the service and remove the installed files
#                 (keeps your config, recordings, stats and logs)
set -euo pipefail

BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
APP_DIR="${APP_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/applications}"
UNIT_DIR="${UNIT_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user}"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
HOOK_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/meeting-recorder/hooks"

build=1
service=1
uninstall=0
for arg in "$@"; do
  case "$arg" in
    --no-build) build=0 ;;
    --no-service) service=0 ;;
    --uninstall) uninstall=1 ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg (see --help)" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")"
have() { command -v "$1" >/dev/null 2>&1; }
refresh_desktop_db() {
  if have update-desktop-database; then update-desktop-database -q "$APP_DIR" || true; fi
  if have kbuildsycoca6; then kbuildsycoca6 >/dev/null 2>&1 || true; fi
}

if [[ $uninstall == 1 ]]; then
  if have systemctl; then
    systemctl --user disable --now meeting-recorder.service 2>/dev/null || true
  fi
  rm -fv "$UNIT_DIR/meeting-recorder.service" "$APP_DIR/meeting-recorder.desktop" \
    "$BIN_DIR/meeting-recorder" "$ICON_DIR/meeting-recorder.svg" \
    "$ICON_DIR/meeting-recorder-recording.svg" "$HOOK_DIR/whisper-transcribe.sh" \
    "$HOOK_DIR/cloud-transcribe.sh"
  rmdir "$HOOK_DIR" 2>/dev/null || true
  if have systemctl; then systemctl --user daemon-reload || true; fi
  refresh_desktop_db
  echo "Uninstalled. Config (~/.config/meeting-recorder), recordings, stats and logs were kept."
  exit 0
fi

for tool in pactl ffmpeg; do
  have "$tool" || echo "warning: required tool '$tool' not found on PATH (see README > Install)" >&2
done

if [[ $build == 1 ]]; then
  cargo build --release --locked
fi
if [[ ! -x target/release/meeting-recorder ]]; then
  echo "target/release/meeting-recorder missing; build first" >&2
  exit 1
fi

mkdir -p "$BIN_DIR" "$APP_DIR"
# Install via a temp file + rename so a running copy of the binary isn't overwritten in place.
install -m 755 target/release/meeting-recorder "$BIN_DIR/.meeting-recorder.new"
mv -f "$BIN_DIR/.meeting-recorder.new" "$BIN_DIR/meeting-recorder"
echo "installed $BIN_DIR/meeting-recorder"

mkdir -p "$HOOK_DIR"
for hook in examples/hooks/*.sh; do
  install -m 755 "$hook" "$HOOK_DIR/$(basename "$hook")"
  echo "installed $HOOK_DIR/$(basename "$hook")"
done

sed "s|^Exec=.*|Exec=$BIN_DIR/meeting-recorder|" packaging/meeting-recorder.desktop \
  > "$APP_DIR/meeting-recorder.desktop"
echo "installed $APP_DIR/meeting-recorder.desktop"
refresh_desktop_db

if [[ $service == 1 ]]; then
  if ! have systemctl; then
    echo "systemctl not found; skipping service setup. Start $BIN_DIR/meeting-recorder at login yourself." >&2
  else
    mkdir -p "$UNIT_DIR"
    sed "s|^ExecStart=.*|ExecStart=$BIN_DIR/meeting-recorder|" packaging/meeting-recorder.service \
      > "$UNIT_DIR/meeting-recorder.service"
    echo "installed $UNIT_DIR/meeting-recorder.service"
    systemctl --user daemon-reload
    systemctl --user enable meeting-recorder.service
    systemctl --user restart meeting-recorder.service
    echo "service (re)started: systemctl --user status meeting-recorder"
  fi
fi

echo "Done. Logs: ~/.local/state/meeting-recorder/logs/  Config: ~/.config/meeting-recorder/config.toml"
