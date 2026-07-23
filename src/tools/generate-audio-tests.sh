#!/usr/bin/env bash
# Regenerate every staged audio test format from volume/test.mp3.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
volume="$root/volume"
source="$volume/test.mp3"

command -v ffmpeg >/dev/null
command -v rustc >/dev/null
[[ -f "$source" ]]

ffmpeg_args=(-nostdin -hide_banner -loglevel error -y)
normalizer="${TMPDIR:-/tmp}/libersystem-normalize-ogg"
trap 'rm -f "$normalizer"' EXIT
ffmpeg "${ffmpeg_args[@]}" -i "$source" -map_metadata -1 -ac 1 -ar 44100 -c:a pcm_s16le "$volume/test.wav"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.wav" -map_metadata -1 -c:a adpcm_ima_wav "$volume/test-ima.wav"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.wav" -map_metadata -1 -c:a adpcm_ms "$volume/test-ms.wav"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.wav" -map_metadata -1 -c:a pcm_s16be "$volume/test.aiff"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.wav" -map_metadata -1 -f aiff -c:a pcm_s16le "$volume/test.aifc"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.wav" -map_metadata -1 -c:a flac -sample_fmt s16 "$volume/test.flac"
ffmpeg "${ffmpeg_args[@]}" -fflags +bitexact -i "$volume/test.wav" -map_metadata -1 -flags:a +bitexact -c:a libvorbis -q:a 5 "$volume/test.ogg"
rustc --edition=2024 -O "$root/tools/normalize-ogg.rs" -o "$normalizer"
"$normalizer" "$volume/test.ogg"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.wav" -map_metadata -1 -c:a wavpack "$volume/test.wv"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.wav" -map_metadata -1 -af 'pan=stereo|c0=c0|c1=-1*c0' -c:a wavpack "$volume/test-stereo.wv"

ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.flac" -f s16le -c:a pcm_s16le "$root/user/libs/flac/tests/data/test-s16le.pcm"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.mp3" -f s16le -c:a pcm_s16le "$root/user/libs/mp3/tests/test.pcm"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.ogg" -f s16le -c:a pcm_s16le "$root/user/libs/vorbis/tests/test.pcm"
ffmpeg "${ffmpeg_args[@]}" -i "$volume/test.wav" -t 0.1 -map_metadata -1 -af 'pan=stereo|c0=c0|c1=-1*c0' -c:a wavpack "$root/user/libs/wavpack/tests/test-stereo-short.wv"
