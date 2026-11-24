#!/usr/bin/env bash
# meeting-recorder pipeline hook: transcribe a recording with a cloud speech-to-text API.
#
# Works with any OpenAI-compatible /audio/transcriptions endpoint (OpenAI, Groq, or a
# self-hosted server). Writes the transcript next to the recording:
#   2026-11-24_12-49-51_Google-Meet.opus  ->  2026-11-24_12-49-51_Google-Meet.txt
#
# Options (put them in the hook's `args` in config.toml):
#   --provider openai|groq|custom   (default: openai)
#   --model NAME          default: gpt-transcribe (openai), whisper-large-v3-turbo (groq)
#   --base-url URL        API base, required for custom (e.g. http://localhost:8000/v1)
#   --api-key-file PATH   file containing only the API key
#                         (default: ~/.config/meeting-recorder/<provider>.key; falls back to
#                         $OPENAI_API_KEY / $GROQ_API_KEY / $TRANSCRIBE_API_KEY)
#   --language CODE|auto  spoken language, e.g. en, uk, de                  (default: auto)
#   --prompt TEXT         names / jargon to help spelling, e.g. "Kubernetes, Anna Kowalski"
#   --chunk-minutes N     split audio into N-minute pieces (keeps uploads under 25 MB;
#                         0 = never split)                                  (default: 20)
#   --min-seconds N       skip recordings shorter than this                 (default: 10)
#
# Can also be run by hand to (re)transcribe an old recording:
#   cloud-transcribe.sh --provider groq rec.opus rec.opus.json
set -euo pipefail

provider=openai
model=
base_url=
key_file=
language=auto
prompt=
chunk_minutes=20
min_seconds=10

usage() { sed -n '2,22p' "$0" >&2; exit 2; }

while [[ $# -gt 2 ]]; do
  case "$1" in
    --provider) provider=$2; shift 2 ;;
    --model) model=$2; shift 2 ;;
    --base-url) base_url=$2; shift 2 ;;
    --api-key-file) key_file=$2; shift 2 ;;
    --language) language=$2; shift 2 ;;
    --prompt) prompt=$2; shift 2 ;;
    --chunk-minutes) chunk_minutes=$2; shift 2 ;;
    --min-seconds) min_seconds=$2; shift 2 ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
done
[[ $# -eq 2 ]] || usage
audio=$1
meta=$2
[[ -f $audio ]] || { echo "audio file not found: $audio" >&2; exit 1; }

config_dir=${XDG_CONFIG_HOME:-$HOME/.config}/meeting-recorder
case "$provider" in
  openai)
    base_url=${base_url:-https://api.openai.com/v1}
    model=${model:-gpt-transcribe}
    env_key=${OPENAI_API_KEY:-} ;;
  groq)
    base_url=${base_url:-https://api.groq.com/openai/v1}
    model=${model:-whisper-large-v3-turbo}
    env_key=${GROQ_API_KEY:-} ;;
  custom)
    [[ -n $base_url && -n $model ]] || { echo "--provider custom needs --base-url and --model" >&2; exit 2; }
    env_key=${TRANSCRIBE_API_KEY:-} ;;
  *) echo "unknown provider: $provider" >&2; usage ;;
esac

# Skip short recordings (test calls, false starts) using the metadata's duration.
duration=$(grep -o '"duration_secs": *[0-9]*' "$meta" 2>/dev/null | grep -o '[0-9]*$' || true)
if [[ -n $duration && $duration -lt $min_seconds ]]; then
  echo "skipping $(basename "$audio"): ${duration}s < --min-seconds $min_seconds"
  exit 0
fi

key_file=${key_file:-$config_dir/$provider.key}
if [[ -f $key_file ]]; then
  api_key=$(tr -d '[:space:]' <"$key_file")
else
  api_key=$env_key
fi
if [[ -z $api_key && $provider != custom ]]; then
  echo "no API key: put it in $key_file (chmod 600) or set the provider's env var" >&2
  exit 1
fi

json_text() {
  if command -v jq >/dev/null 2>&1; then
    jq -r '.text // empty'
  else
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("text", ""))'
  fi
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
chmod 700 "$work"

# The key goes to curl via a header file, so it never shows up in `ps` or in logs.
headers="$work/headers"
: >"$headers"
chmod 600 "$headers"
if [[ -n $api_key ]]; then printf 'Authorization: Bearer %s\n' "$api_key" >"$headers"; fi

# Speech only needs 16 kHz mono; Opus at 24 kbit/s is ~11 MB per hour and accepted by both
# OpenAI and Groq (as .ogg). Split into chunks so each upload stays well under 25 MB.
segment_args=()
if [[ $chunk_minutes -gt 0 ]]; then
  segment_args=(-f segment -segment_time $((chunk_minutes * 60)) -reset_timestamps 1)
fi
if [[ $chunk_minutes -gt 0 ]]; then
  ffmpeg -nostdin -v error -y -i "$audio" -vn -ac 1 -ar 16000 -c:a libopus -b:a 24k \
    "${segment_args[@]}" "$work/chunk_%03d.ogg"
else
  ffmpeg -nostdin -v error -y -i "$audio" -vn -ac 1 -ar 16000 -c:a libopus -b:a 24k \
    "$work/chunk_000.ogg"
fi

lang_args=()
if [[ $language != auto ]]; then
  # gpt-transcribe takes a list of languages; older models a single `language`.
  if [[ $model == gpt-transcribe* ]]; then
    lang_args=(-F "languages[]=$language")
  else
    lang_args=(-F "language=$language")
  fi
fi
prompt_args=()
if [[ -n $prompt ]]; then prompt_args=(--form-string "prompt=$prompt"); fi

name=$(basename "$audio")
out="$(dirname "$audio")/${name%.*}.txt"
chunks=("$work"/chunk_*.ogg)
echo "transcribing $name with $provider/$model (${#chunks[@]} chunk(s))"

: >"$work/transcript.txt"
for chunk in "${chunks[@]}"; do
  response=$(curl -sS --fail-with-body --retry 3 --retry-delay 5 --max-time 900 \
    -H @"$headers" \
    -F "file=@$chunk;type=audio/ogg" \
    -F "model=$model" \
    -F "response_format=json" \
    "${lang_args[@]}" "${prompt_args[@]}" \
    "$base_url/audio/transcriptions") || {
      echo "API request failed for $(basename "$chunk"): $response" >&2
      exit 1
    }
  printf '%s\n' "$(json_text <<<"$response")" >>"$work/transcript.txt"
done

mv "$work/transcript.txt" "$out"
echo "done: $out"
