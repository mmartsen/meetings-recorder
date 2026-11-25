#!/usr/bin/env bash
# meeting-recorder pipeline hook: transcribe a recording with a cloud speech-to-text API.
#
# Works with any OpenAI-compatible /audio/transcriptions endpoint (OpenAI, Groq, OpenRouter,
# or a self-hosted server). Writes the transcript next to the recording:
#   2026-11-24_12-49-51_Google-Meet.opus  ->  2026-11-24_12-49-51_Google-Meet.txt (/.srt/.vtt)
#
# Options (put them in the hook's `args` in config.toml):
#   --provider openai|groq|openrouter|custom   (default: openai)
#   --model NAME          default: gpt-transcribe (openai), whisper-large-v3-turbo (groq),
#                         openai/gpt-transcribe (openrouter)
#   --base-url URL        API base, required for custom (e.g. http://localhost:8000/v1)
#   --api-key-file PATH   file containing only the API key
#                         (default: ~/.config/meeting-recorder/<provider>.key; falls back to
#                         $OPENAI_API_KEY / $GROQ_API_KEY / $OPENROUTER_API_KEY /
#                         $TRANSCRIBE_API_KEY)
#   --language CODE|auto  spoken language, e.g. en, uk, de                  (default: auto)
#   --prompt TEXT         names / jargon to help spelling, e.g. "Kubernetes, Anna Kowalski"
#   --chunk-minutes N     split audio into N-minute pieces (keeps uploads under 25 MB;
#                         0 = never split)                                  (default: 20)
#   --min-seconds N       skip recordings shorter than this                 (default: 10)
#   --formats LIST        comma-separated: txt,srt,vtt                      (default: txt)
#                         srt/vtt need segment timestamps (verbose_json), which whisper-1 and
#                         whisper-large-v3* have but gpt-*-transcribe models don't; with them,
#                         the .txt gets one segment per line instead of one line per chunk.
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
formats=txt

usage() { sed -n '2,28p' "$0" >&2; exit 2; }

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
    --formats) formats=$2; shift 2 ;;
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
  openrouter)
    # OpenRouter routes to OpenAI's STT models; names need the vendor prefix
    # (openai/gpt-transcribe, openai/whisper-1). It refuses audio below a $0.50 balance.
    base_url=${base_url:-https://openrouter.ai/api/v1}
    model=${model:-openai/gpt-transcribe}
    env_key=${OPENROUTER_API_KEY:-} ;;
  custom)
    [[ -n $base_url && -n $model ]] || { echo "--provider custom needs --base-url and --model" >&2; exit 2; }
    env_key=${TRANSCRIBE_API_KEY:-} ;;
  *) echo "unknown provider: $provider" >&2; usage ;;
esac

# srt/vtt are built from the segments of a verbose_json response.
timestamps=0
IFS=, read -ra fmts <<<"$formats"
for f in "${fmts[@]}"; do
  case "$f" in
    txt) ;;
    srt|vtt) timestamps=1 ;;
    *) echo "unsupported format: $f (use txt, srt, vtt)" >&2; exit 2 ;;
  esac
done
want() { [[ ,$formats, == *,$1,* ]]; }
# Fail before uploading anything: these models only return plain text.
if [[ $timestamps -eq 1 && ${model#*/} == gpt-*transcribe* ]]; then
  echo "$model can't return timestamps, so it can't produce srt/vtt;" \
    "use --model whisper-1 (openrouter: openai/whisper-1) or --formats txt" >&2
  exit 2
fi

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

# verbose_json -> "start<TAB>end<TAB>text" lines, shifted by $1 seconds (the chunk's offset in
# the recording). Fails if the response has no segments.
json_segments() {
  if command -v jq >/dev/null 2>&1; then
    jq -er --argjson off "$1" '(.segments // error("no segments")) | .[]
      | [(.start + $off), (.end + $off), (.text | gsub("[\t\r\n]+"; " ") | sub("^ +"; ""))]
      | @tsv'
  else
    python3 -c '
import json, re, sys
off = float(sys.argv[1])
for s in json.load(sys.stdin)["segments"]:
    text = re.sub(r"[\t\r\n]+", " ", s["text"]).lstrip()
    print("%s\t%s\t%s" % (s["start"] + off, s["end"] + off, text))' "$1"
  fi
}

# Render the segment TSV as srt (sep ",") or vtt (sep "."; header added by the caller).
render_cues() {
  awk -F'\t' -v sep="$1" '
    function ts(t,  ms) { ms = int(t * 1000 + 0.5)
      return sprintf("%02d:%02d:%02d%s%03d", int(ms / 3600000), int(ms / 60000) % 60,
                     int(ms / 1000) % 60, sep, ms % 1000) }
    $3 != "" { n++; if (sep == ",") print n; print ts($1) " --> " ts($2); print $3; print "" }'
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
# OpenAI, Groq and OpenRouter (as .ogg). Split into chunks so each upload stays well under 25 MB.
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

format_args=(-F "response_format=json")
if [[ $timestamps -eq 1 ]]; then
  format_args=(-F "response_format=verbose_json" -F "timestamp_granularities[]=segment")
fi

lang_args=()
if [[ $language != auto ]]; then
  # gpt-transcribe takes a list of languages; older models a single `language`.
  if [[ ${model#*/} == gpt-transcribe* ]]; then
    lang_args=(-F "languages[]=$language")
  else
    lang_args=(-F "language=$language")
  fi
fi
prompt_args=()
if [[ -n $prompt ]]; then prompt_args=(--form-string "prompt=$prompt"); fi

name=$(basename "$audio")
base="$(dirname "$audio")/${name%.*}"
chunks=("$work"/chunk_*.ogg)
echo "transcribing $name with $provider/$model (${#chunks[@]} chunk(s))"

: >"$work/transcript.txt"
: >"$work/segments.tsv"
offset=0
for chunk in "${chunks[@]}"; do
  response=$(curl -sS --fail-with-body --retry 3 --retry-delay 5 --max-time 900 \
    -H @"$headers" \
    -F "file=@$chunk;type=audio/ogg" \
    -F "model=$model" \
    "${format_args[@]}" "${lang_args[@]}" "${prompt_args[@]}" \
    "$base_url/audio/transcriptions") || {
      echo "API request failed for $(basename "$chunk"): $response" >&2
      exit 1
    }
  if [[ $timestamps -eq 1 ]]; then
    json_segments "$offset" <<<"$response" >>"$work/segments.tsv" || {
      echo "no segment timestamps in the response for $(basename "$chunk");" \
        "$model probably doesn't support verbose_json, use --formats txt" >&2
      exit 1
    }
    # Use the chunk's real length, not chunk_minutes, so later timestamps don't drift.
    len=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$chunk")
    offset=$(awk -v a="$offset" -v b="$len" 'BEGIN { printf "%.3f", a + b }')
  else
    printf '%s\n' "$(json_text <<<"$response")" >>"$work/transcript.txt"
  fi
done

if [[ $timestamps -eq 1 ]]; then
  # A chunk's last segment can end past the chunk (Whisper rounds up), overlapping the next
  # chunk's first cue: cap every end at the next start.
  awk -F'\t' -v OFS='\t' '{ s[NR] = $1; e[NR] = $2; t[NR] = $3 }
    END { for (i = 1; i <= NR; i++) {
            if (i < NR && e[i] > s[i + 1]) e[i] = s[i + 1]
            print s[i], e[i], t[i] } }' "$work/segments.tsv" >"$work/segments.clamped.tsv"
  mv "$work/segments.clamped.tsv" "$work/segments.tsv"
  cut -f3 "$work/segments.tsv" | grep -v '^$' >"$work/transcript.txt" || true
  render_cues , <"$work/segments.tsv" >"$work/transcript.srt"
  { printf 'WEBVTT\n\n'; render_cues . <"$work/segments.tsv"; } >"$work/transcript.vtt"
fi

# Move results into place only now, so a failed run never overwrites an existing transcript.
written=()
for f in txt srt vtt; do
  if want "$f"; then mv "$work/transcript.$f" "$base.$f"; written+=("$base.$f"); fi
done
echo "done: ${written[*]}"
