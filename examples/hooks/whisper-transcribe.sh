#!/usr/bin/env bash
# meeting-recorder pipeline hook: transcribe a recording with Whisper.
#
# meeting-recorder calls hooks as:  <command> <args...> <audio_file> <metadata_json>
# and writes the transcript(s) next to the recording, e.g.
#   2026-11-24_12-49-51_Google-Meet.opus  ->  2026-11-24_12-49-51_Google-Meet.txt / .srt
#
# Options (put them in the hook's `args` in config.toml):
#   --backend auto|whisper.cpp|faster-whisper|openai   (default: auto, first one installed)
#   --model NAME|PATH     model name (tiny, base, small, medium, large-v3, *.en ...) or, for
#                         whisper.cpp, a path to a ggml .bin file            (default: base)
#   --language CODE|auto  spoken language, e.g. en, uk, de                   (default: auto)
#   --threads N           CPU threads                                        (default: nproc)
#   --formats LIST        comma-separated: txt,srt,vtt,json                  (default: txt,srt)
#   --min-seconds N       skip recordings shorter than this                  (default: 10)
#   --force               transcribe again even if the transcript files already exist
#   --vad auto|on|off     whisper.cpp only: run voice activity detection first, so silence
#                         never reaches Whisper (stops it inventing "thanks for watching" /
#                         subtitle credits). auto = on if the VAD model exists (default: auto)
#   --vad-model PATH      Silero VAD model for whisper.cpp
#                         (default: <models dir>/ggml-silero-v5.1.2.bin)
#
# Can also be run by hand to (re)transcribe an old recording:
#   whisper-transcribe.sh --model small rec.opus rec.opus.json
# Recordings whose requested formats all exist already are skipped (see --force).
set -euo pipefail

backend=auto
model=base
language=auto
threads=$(nproc 2>/dev/null || echo 4)
formats=txt,srt
min_seconds=10
force=0
vad=auto
vad_model=

usage() { sed -n '2,25p' "$0" >&2; exit 2; }

while [[ $# -gt 2 ]]; do
  case "$1" in
    --backend) backend=$2; shift 2 ;;
    --model) model=$2; shift 2 ;;
    --language) language=$2; shift 2 ;;
    --threads) threads=$2; shift 2 ;;
    --formats) formats=$2; shift 2 ;;
    --min-seconds) min_seconds=$2; shift 2 ;;
    --force) force=1; shift ;;
    --vad) vad=$2; shift 2 ;;
    --vad-model) vad_model=$2; shift 2 ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
done
[[ $# -eq 2 ]] || usage
audio=$1
meta=$2
[[ -f $audio ]] || { echo "audio file not found: $audio" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# Skip short recordings (test calls, false starts) using the metadata's duration.
duration=$(grep -o '"duration_secs": *[0-9]*' "$meta" 2>/dev/null | grep -o '[0-9]*$' || true)
if [[ -n $duration && $duration -lt $min_seconds ]]; then
  echo "skipping $(basename "$audio"): ${duration}s < --min-seconds $min_seconds"
  exit 0
fi

dir=$(dirname "$audio")
name=$(basename "$audio")
base="$dir/${name%.*}"

# One run per recording at a time (a pipeline run and a manual re-run, or both hooks), keyed
# on the resolved path so relative and absolute invocations share the lock.
key=$(realpath "$audio" | sha1sum | cut -c1-16)
exec 8>"${XDG_RUNTIME_DIR:-/tmp}/meeting-recorder-transcribe-$key.lock"
flock 8

# Already transcribed? Outputs are only ever moved into place complete, so an existing file
# means a previous run finished (an empty one is a recording with no speech in it).
if [[ $force -eq 0 ]]; then
  missing=0
  IFS=, read -ra want_fmts <<<"$formats"
  for f in "${want_fmts[@]}"; do [[ -e $base.$f ]] || missing=1; done
  if [[ $missing -eq 0 ]]; then
    echo "skipping $name: already transcribed ($formats exist; use --force to redo)"
    exit 0
  fi
fi

# Copy into place via a temp name in the same directory, then rename: a killed run never
# leaves a half-written transcript that the check above would mistake for a finished one.
place() { cp "$1" "$2.partial.$$" && mv -f "$2.partial.$$" "$2"; }

if [[ $backend == auto ]]; then
  if have whisper-cli || have whisper-cpp; then backend=whisper.cpp
  elif have whisper-ctranslate2; then backend=faster-whisper
  elif have whisper; then backend=openai
  else
    echo "no Whisper found on PATH (whisper-cli, whisper-ctranslate2 or whisper); see README" >&2
    exit 1
  fi
fi

# One transcription at a time: Whisper saturates the CPU/GPU, and two calls can end close
# together.
lock="${XDG_RUNTIME_DIR:-/tmp}/meeting-recorder-whisper.lock"
exec 9>"$lock"
flock 9

echo "transcribing $name with $backend (model $model, language $language, vad $vad)"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
out="$work/${name%.*}"

case "$backend" in
  whisper.cpp)
    bin=$(command -v whisper-cli || command -v whisper-cpp) || { echo "whisper-cli not found" >&2; exit 1; }
    models_dir=${WHISPER_CPP_MODELS:-${XDG_DATA_HOME:-$HOME/.local/share}/whisper.cpp/models}
    model_file=$model
    [[ -f $model_file ]] || model_file="$models_dir/ggml-$model.bin"
    if [[ ! -f $model_file ]]; then
      echo "whisper.cpp model not found: $model_file" >&2
      echo "download it with:" >&2
      echo "  mkdir -p '$(dirname "$model_file")' && curl -L -o '$model_file' https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-$model.bin" >&2
      exit 1
    fi
    # -mc 0: don't feed earlier text back in as context, which is what lets Whisper fall into
    # repeating one line for minutes. -sns: suppress non-speech tokens.
    flags=(-mc 0 -sns)
    vad_model=${vad_model:-$models_dir/ggml-silero-v5.1.2.bin}
    case "$vad" in
      auto|on)
        if [[ -f $vad_model ]]; then
          flags+=(--vad -vm "$vad_model")
        elif [[ $vad == on ]]; then
          echo "VAD model not found: $vad_model" >&2
          echo "download it with:" >&2
          echo "  curl -L -o '$vad_model' https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin" >&2
          exit 1
        else
          echo "note: no VAD model at $vad_model; silent parts may produce invented text (see README)"
        fi ;;
      off) ;;
      *) echo "--vad must be auto, on or off" >&2; exit 2 ;;
    esac
    # whisper.cpp wants 16 kHz mono WAV; recordings are Opus/MP3.
    wav="$work/audio.wav"
    ffmpeg -nostdin -v error -y -i "$audio" -ar 16000 -ac 1 -c:a pcm_s16le "$wav"
    IFS=, read -ra fmts <<<"$formats"
    for f in "${fmts[@]}"; do
      case "$f" in
        txt) flags+=(-otxt) ;; srt) flags+=(-osrt) ;; vtt) flags+=(-ovtt) ;; json) flags+=(-oj) ;;
        *) echo "unsupported format: $f" >&2; exit 2 ;;
      esac
    done
    "$bin" -m "$model_file" -f "$wav" -l "$language" -t "$threads" -np "${flags[@]}" -of "$out"
    ;;

  faster-whisper|openai)
    if [[ $backend == faster-whisper ]]; then bin=whisper-ctranslate2; else bin=whisper; fi
    have "$bin" || { echo "$bin not found on PATH" >&2; exit 1; }
    lang_args=()
    [[ $language == auto ]] || lang_args=(--language "$language")
    # Both CLIs write <name-without-ext>.<fmt> into --output_dir. Write all formats once
    # (cheaper than one Whisper run per format); only the requested ones are placed below.
    "$bin" "$audio" --model "$model" --threads "$threads" "${lang_args[@]}" \
      --output_dir "$work" --output_format all
    ;;

  *) echo "unknown backend: $backend" >&2; usage ;;
esac

written=()
for f in txt srt vtt json; do
  [[ ,$formats, == *,$f,* ]] || continue
  [[ -f $out.$f ]] || { echo "Whisper produced no .$f output" >&2; exit 1; }
  place "$out.$f" "$base.$f"
  written+=("$base.$f")
done
echo "done: ${written[*]}"
