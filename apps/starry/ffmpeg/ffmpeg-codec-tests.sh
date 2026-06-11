#!/bin/sh
set -eu

test_done=0

cleanup() {
    rm -rf /tmp/ffmpeg-codec-* 2>/dev/null || true
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ]; then
        echo "FFMPEG_CODEC_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

fail() {
    echo "FFMPEG_CODEC_STAGE FAILED: $1"
    echo "FFMPEG_CODEC_TEST_FAILED"
    exit 1
}

mkdir -p /tmp/ffmpeg-codec-workdir

# ---- Generate test media if not pre-built ----
. /usr/bin/ffmpeg-ensure-media.sh
TEST_MEDIA_DIR="$FFMPEG_TEST_MEDIA_DIR"

has_encoder() {
    ffmpeg -encoders 2>/dev/null | grep -q "$1"
}

has_decoder() {
    ffmpeg -decoders 2>/dev/null | grep -q "$1"
}

gen_raw_video() {
    local out="$1"
    local w="${2:-160}"
    local h="${3:-120}"
    local d="${4:-1}"
    ffmpeg -y -f lavfi -i "color=c=red:s=${w}x${h}:d=${d}" \
        -c:v rawvideo -pix_fmt yuv420p \
        -frames:v 10 \
        "$out" 2>/dev/null
}

gen_raw_audio() {
    local out="$1"
    local d="${2:-2}"
    ffmpeg -y -f lavfi -i "sine=frequency=440:duration=${d}" \
        -c:a pcm_s16le \
        "$out" 2>/dev/null
}

# ===== VIDEO CODECS =====

# ---- H.264 (libx264) encode/decode ----
echo "FFMPEG_CODEC_STAGE h264-encode"
has_encoder "libx264" || fail "encoder libx264 not available"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
        /tmp/ffmpeg-codec-workdir/h264.mp4 2>/dev/null \
        || fail "h264 encode failed"
else
    gen_raw_video /tmp/ffmpeg-codec-workdir/raw.yuv || fail "cannot generate raw video"
    ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw.yuv \
        -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
        /tmp/ffmpeg-codec-workdir/h264.mp4 2>/dev/null \
        || fail "h264 encode from raw failed"
fi
[ -s /tmp/ffmpeg-codec-workdir/h264.mp4 ] || fail "h264 output is empty"

echo "FFMPEG_CODEC_STAGE h264-decode"
has_decoder "h264" || fail "decoder h264 not available"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/h264.mp4 \
    -f rawvideo -pix_fmt yuv420p \
    /tmp/ffmpeg-codec-workdir/h264_decoded.yuv 2>/dev/null \
    || fail "h264 decode failed"
[ -s /tmp/ffmpeg-codec-workdir/h264_decoded.yuv ] || fail "h264 decoded output is empty"

# ---- MPEG-4 encode/decode ----
echo "FFMPEG_CODEC_STAGE mpeg4-encode"
has_encoder "mpeg4" || fail "encoder mpeg4 not available"
gen_raw_video /tmp/ffmpeg-codec-workdir/raw_mpeg4.yuv || fail "cannot generate raw video"
ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_mpeg4.yuv \
    -c:v mpeg4 -q:v 10 \
    /tmp/ffmpeg-codec-workdir/mpeg4.avi 2>/dev/null \
    || fail "mpeg4 encode failed"
[ -s /tmp/ffmpeg-codec-workdir/mpeg4.avi ] || fail "mpeg4 output is empty"

echo "FFMPEG_CODEC_STAGE mpeg4-decode"
has_decoder "mpeg4" || fail "decoder mpeg4 not available"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/mpeg4.avi \
    -f rawvideo -pix_fmt yuv420p \
    /tmp/ffmpeg-codec-workdir/mpeg4_decoded.yuv 2>/dev/null \
    || fail "mpeg4 decode failed"
[ -s /tmp/ffmpeg-codec-workdir/mpeg4_decoded.yuv ] || fail "mpeg4 decoded output is empty"

# ---- VP8 encode/decode ----
echo "FFMPEG_CODEC_STAGE vp8-encode"
has_encoder "libvpx" || fail "encoder libvpx not available"
gen_raw_video /tmp/ffmpeg-codec-workdir/raw_vp8.yuv 160 120 1 || fail "cannot generate raw video"
ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_vp8.yuv \
    -c:v libvpx -quality realtime -cpu-used 4 \
    /tmp/ffmpeg-codec-workdir/vp8.webm 2>/dev/null \
    || fail "vp8 encode failed"
[ -s /tmp/ffmpeg-codec-workdir/vp8.webm ] || fail "vp8 output is empty"

echo "FFMPEG_CODEC_STAGE vp8-decode"
has_decoder "vp8" || fail "decoder vp8 not available"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/vp8.webm \
    -f rawvideo -pix_fmt yuv420p \
    /tmp/ffmpeg-codec-workdir/vp8_decoded.yuv 2>/dev/null \
    || fail "vp8 decode failed"
[ -s /tmp/ffmpeg-codec-workdir/vp8_decoded.yuv ] || fail "vp8 decoded output is empty"

# ---- VP9 encode ----
echo "FFMPEG_CODEC_STAGE vp9-encode"
has_encoder "libvpx-vp9" || fail "encoder libvpx-vp9 not available"
gen_raw_video /tmp/ffmpeg-codec-workdir/raw_vp9.yuv 160 120 1 || fail "cannot generate raw video"
ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_vp9.yuv \
    -c:v libvpx-vp9 -quality realtime -cpu-used 4 \
    /tmp/ffmpeg-codec-workdir/vp9.webm 2>/dev/null \
    || fail "vp9 encode failed"
[ -s /tmp/ffmpeg-codec-workdir/vp9.webm ] || fail "vp9 output is empty"

# ---- MJPEG encode/decode ----
echo "FFMPEG_CODEC_STAGE mjpeg-encode"
has_encoder "mjpeg" || fail "encoder mjpeg not available"
gen_raw_video /tmp/ffmpeg-codec-workdir/raw_mjpeg.yuv || fail "cannot generate raw video"
ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_mjpeg.yuv \
    -c:v mjpeg -q:v 5 \
    /tmp/ffmpeg-codec-workdir/mjpeg.avi 2>/dev/null \
    || fail "mjpeg encode failed"
[ -s /tmp/ffmpeg-codec-workdir/mjpeg.avi ] || fail "mjpeg output is empty"

echo "FFMPEG_CODEC_STAGE mjpeg-decode"
has_decoder "mjpeg" || fail "decoder mjpeg not available"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/mjpeg.avi \
    -f rawvideo -pix_fmt yuv420p \
    /tmp/ffmpeg-codec-workdir/mjpeg_decoded.yuv 2>/dev/null \
    || fail "mjpeg decode failed"
[ -s /tmp/ffmpeg-codec-workdir/mjpeg_decoded.yuv ] || fail "mjpeg decoded output is empty"

# ---- Raw video ----
echo "FFMPEG_CODEC_STAGE rawvideo"
gen_raw_video /tmp/ffmpeg-codec-workdir/raw_test.yuv || fail "cannot generate raw video"
ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_test.yuv \
    -c:v rawvideo -pix_fmt yuv420p \
    /tmp/ffmpeg-codec-workdir/raw_out.yuv 2>/dev/null \
    || fail "rawvideo copy failed"
[ -s /tmp/ffmpeg-codec-workdir/raw_out.yuv ] || fail "rawvideo output is empty"

# ===== AUDIO CODECS =====

# ---- MP3 (libmp3lame) encode/decode ----
echo "FFMPEG_CODEC_STAGE mp3-encode"
has_encoder "libmp3lame" || fail "encoder libmp3lame not available"
gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_audio.wav 2 || fail "cannot generate raw audio"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_audio.wav \
    -c:a libmp3lame -b:a 128k \
    /tmp/ffmpeg-codec-workdir/audio.mp3 2>/dev/null \
    || fail "mp3 encode failed"
[ -s /tmp/ffmpeg-codec-workdir/audio.mp3 ] || fail "mp3 output is empty"

echo "FFMPEG_CODEC_STAGE mp3-decode"
has_decoder "mp3" || fail "decoder mp3 not available"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/audio.mp3 \
    -c:a pcm_s16le \
    /tmp/ffmpeg-codec-workdir/mp3_decoded.wav 2>/dev/null \
    || fail "mp3 decode failed"
[ -s /tmp/ffmpeg-codec-workdir/mp3_decoded.wav ] || fail "mp3 decoded output is empty"

# ---- AAC encode/decode ----
echo "FFMPEG_CODEC_STAGE aac-encode"
has_encoder "aac" || fail "encoder aac not available"
gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_aac.wav 2 || fail "cannot generate raw audio"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_aac.wav \
    -c:a aac -b:a 128k \
    /tmp/ffmpeg-codec-workdir/audio.aac 2>/dev/null \
    || fail "aac encode failed"
[ -s /tmp/ffmpeg-codec-workdir/audio.aac ] || fail "aac output is empty"

echo "FFMPEG_CODEC_STAGE aac-decode"
has_decoder "aac" || fail "decoder aac not available"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/audio.aac \
    -c:a pcm_s16le \
    /tmp/ffmpeg-codec-workdir/aac_decoded.wav 2>/dev/null \
    || fail "aac decode failed"
[ -s /tmp/ffmpeg-codec-workdir/aac_decoded.wav ] || fail "aac decoded output is empty"

# ---- Vorbis encode ----
echo "FFMPEG_CODEC_STAGE vorbis-encode"
has_encoder "libvorbis" || fail "encoder libvorbis not available"
gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_vorbis.wav 2 || fail "cannot generate raw audio"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_vorbis.wav \
    -c:a libvorbis -b:a 128k \
    /tmp/ffmpeg-codec-workdir/audio.ogg 2>/dev/null \
    || fail "vorbis encode failed"
[ -s /tmp/ffmpeg-codec-workdir/audio.ogg ] || fail "vorbis output is empty"

# ---- Opus encode/decode ----
echo "FFMPEG_CODEC_STAGE opus-encode"
has_encoder "libopus" || fail "encoder libopus not available"
gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_opus.wav 2 || fail "cannot generate raw audio"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_opus.wav \
    -c:a libopus -b:a 64k \
    /tmp/ffmpeg-codec-workdir/audio.opus 2>/dev/null \
    || fail "opus encode failed"
[ -s /tmp/ffmpeg-codec-workdir/audio.opus ] || fail "opus output is empty"

echo "FFMPEG_CODEC_STAGE opus-decode"
has_decoder "opus" || fail "decoder opus not available"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/audio.opus \
    -c:a pcm_s16le \
    /tmp/ffmpeg-codec-workdir/opus_decoded.wav 2>/dev/null \
    || fail "opus decode failed"
[ -s /tmp/ffmpeg-codec-workdir/opus_decoded.wav ] || fail "opus decoded output is empty"

# ---- FLAC encode/decode ----
echo "FFMPEG_CODEC_STAGE flac-encode"
has_encoder "flac" || fail "encoder flac not available"
gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_flac.wav 2 || fail "cannot generate raw audio"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_flac.wav \
    -c:a flac \
    /tmp/ffmpeg-codec-workdir/audio.flac 2>/dev/null \
    || fail "flac encode failed"
[ -s /tmp/ffmpeg-codec-workdir/audio.flac ] || fail "flac output is empty"

echo "FFMPEG_CODEC_STAGE flac-decode"
has_decoder "flac" || fail "decoder flac not available"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/audio.flac \
    -c:a pcm_s16le \
    /tmp/ffmpeg-codec-workdir/flac_decoded.wav 2>/dev/null \
    || fail "flac decode failed"
[ -s /tmp/ffmpeg-codec-workdir/flac_decoded.wav ] || fail "flac decoded output is empty"

# ===== CONTAINER FORMATS =====

# ---- MKV container ----
echo "FFMPEG_CODEC_STAGE mkv-container"
[ -f "$TEST_MEDIA_DIR/test_160x120.mkv" ] || fail "test media test_160x120.mkv missing"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mkv" \
    -c copy \
    /tmp/ffmpeg-codec-workdir/remuxed.mkv 2>/dev/null \
    || fail "mkv remux failed"
[ -s /tmp/ffmpeg-codec-workdir/remuxed.mkv ] || fail "mkv remuxed output is empty"

# ---- AVI container ----
echo "FFMPEG_CODEC_STAGE avi-container"
[ -f "$TEST_MEDIA_DIR/test_160x120.avi" ] || fail "test media test_160x120.avi missing"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.avi" \
    -c copy \
    /tmp/ffmpeg-codec-workdir/remuxed.avi 2>/dev/null \
    || fail "avi remux failed"
[ -s /tmp/ffmpeg-codec-workdir/remuxed.avi ] || fail "avi remuxed output is empty"

# ---- WebM container ----
echo "FFMPEG_CODEC_STAGE webm-container"
has_encoder "libvpx" || fail "encoder libvpx not available for webm"
gen_raw_video /tmp/ffmpeg-codec-workdir/raw_webm.yuv 160 120 1 || fail "cannot generate raw video"
ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_webm.yuv \
    -c:v libvpx -quality realtime -cpu-used 4 \
    /tmp/ffmpeg-codec-workdir/test.webm 2>/dev/null \
    || fail "webm encode failed"
[ -s /tmp/ffmpeg-codec-workdir/test.webm ] || fail "webm output is empty"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/test.webm \
    -c copy \
    /tmp/ffmpeg-codec-workdir/remuxed.webm 2>/dev/null \
    || fail "webm remux failed"

# ---- Cross-container transcoding ----
echo "FFMPEG_CODEC_STAGE cross-container"
[ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ] || fail "test media test_160x120.mp4 missing"
has_encoder "libvpx" || fail "encoder libvpx not available for cross-container"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -c:v libvpx -quality realtime -cpu-used 4 \
    /tmp/ffmpeg-codec-workdir/cross.webm 2>/dev/null \
    || fail "cross-container mp4->webm failed"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/cross.webm \
    -c copy \
    /tmp/ffmpeg-codec-workdir/cross.mkv 2>/dev/null \
    || fail "cross-container webm->mkv failed"
[ -s /tmp/ffmpeg-codec-workdir/cross.mkv ] || fail "cross-container output is empty"

# ---- H.265 (libx265) encode/decode ----
echo "FFMPEG_CODEC_STAGE h265-encode"
has_encoder "libx265" || fail "encoder libx265 not available"
gen_raw_video /tmp/ffmpeg-codec-workdir/raw_h265.yuv 160 120 1 || fail "cannot generate raw video"
ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_h265.yuv \
    -c:v libx265 -preset ultrafast -pix_fmt yuv420p \
    /tmp/ffmpeg-codec-workdir/h265.mp4 2>/dev/null \
    || fail "h265 encode failed"
[ -s /tmp/ffmpeg-codec-workdir/h265.mp4 ] || fail "h265 output is empty"

echo "FFMPEG_CODEC_STAGE h265-decode"
has_decoder "hevc" || fail "decoder hevc not available"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/h265.mp4 \
    -f rawvideo -pix_fmt yuv420p \
    /tmp/ffmpeg-codec-workdir/h265_decoded.yuv 2>/dev/null \
    || fail "h265 decode failed"
[ -s /tmp/ffmpeg-codec-workdir/h265_decoded.yuv ] || fail "h265 decoded output is empty"

# ---- Audio sample format conversion ----
echo "FFMPEG_CODEC_STAGE sample-fmt-convert"
gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_s16.wav 2 || fail "cannot generate raw audio"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_s16.wav \
    -c:a pcm_f32le \
    /tmp/ffmpeg-codec-workdir/output_f32.wav 2>/dev/null \
    || fail "sample format conversion failed"
[ -s /tmp/ffmpeg-codec-workdir/output_f32.wav ] || fail "f32 output is empty"

# ---- Audio bit rate ladder ----
echo "FFMPEG_CODEC_STAGE bitrate-ladder"
gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_bl.wav 2 || fail "cannot generate raw audio"
for br in 64 128 192 256; do
    ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_bl.wav \
        -c:a libmp3lame -b:a "${br}k" \
        "/tmp/ffmpeg-codec-workdir/audio_${br}k.mp3" 2>/dev/null \
        || fail "mp3 encode at ${br}k failed"
    [ -s "/tmp/ffmpeg-codec-workdir/audio_${br}k.mp3" ] || fail "mp3 at ${br}k is empty"
done

# ---- Mux separate video + audio ----
echo "FFMPEG_CODEC_STAGE mux-av"
[ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ] || fail "test media test_160x120.mp4 missing"
[ -f "$TEST_MEDIA_DIR/test_audio.mp3" ] || fail "test media test_audio.mp3 missing"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" -i "$TEST_MEDIA_DIR/test_audio.mp3" \
    -c:v copy -c:a aac -shortest \
    /tmp/ffmpeg-codec-workdir/muxed_av.mp4 2>/dev/null \
    || fail "mux video+audio failed"
[ -s /tmp/ffmpeg-codec-workdir/muxed_av.mp4 ] || fail "muxed output is empty"
ffprobe -v quiet -show_entries stream=codec_type \
    /tmp/ffmpeg-codec-workdir/muxed_av.mp4 2>&1 | grep -q "video" \
    || fail "video stream missing in muxed file"
ffprobe -v quiet -show_entries stream=codec_type \
    /tmp/ffmpeg-codec-workdir/muxed_av.mp4 2>&1 | grep -q "audio" \
    || fail "audio stream missing in muxed file"

# ---- Video resolution ladder ----
echo "FFMPEG_CODEC_STAGE resolution-ladder"
[ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ] || fail "test media test_160x120.mp4 missing"
for res in "80:60" "160:120"; do
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -vf "scale=$res" -c:v libx264 -preset ultrafast -frames:v 3 \
        "/tmp/ffmpeg-codec-workdir/res_$(echo $res | tr ':' 'x').mp4" 2>/dev/null \
        || fail "resolution ladder at $res failed"
    [ -s "/tmp/ffmpeg-codec-workdir/res_$(echo $res | tr ':' 'x').mp4" ] || fail "output at $res is empty"
done

test_done=1
trap - EXIT
cleanup

echo "FFMPEG_CODEC_TEST_PASSED"
