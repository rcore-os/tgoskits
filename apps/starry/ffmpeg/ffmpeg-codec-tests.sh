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

skip() {
    echo "FFMPEG_CODEC_STAGE $1 SKIP: $2"
}

mkdir -p /tmp/ffmpeg-codec-workdir

TEST_MEDIA_DIR="/usr/share/ffmpeg-test-media"

# ---- Helper: check if a codec is available ----
has_encoder() {
    ffmpeg -encoders 2>/dev/null | grep -q "$1"
}

has_decoder() {
    ffmpeg -decoders 2>/dev/null | grep -q "$1"
}

# ---- Helper: generate raw test source ----
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

# ---- Stage 1: H.264 (libx264) encode/decode ----
echo "FFMPEG_CODEC_STAGE h264-encode"
if has_encoder "libx264"; then
    if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
        ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
            -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
            /tmp/ffmpeg-codec-workdir/h264.mp4 2>/dev/null \
            || fail "h264 encode failed"
        [ -s /tmp/ffmpeg-codec-workdir/h264.mp4 ] || fail "h264 output is empty"
    else
        gen_raw_video /tmp/ffmpeg-codec-workdir/raw.yuv || fail "cannot generate raw video"
        ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw.yuv \
            -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
            /tmp/ffmpeg-codec-workdir/h264.mp4 2>/dev/null \
            || fail "h264 encode from raw failed"
        [ -s /tmp/ffmpeg-codec-workdir/h264.mp4 ] || fail "h264 output is empty"
    fi
else
    skip "h264-encode" "libx264 not available"
fi

echo "FFMPEG_CODEC_STAGE h264-decode"
if has_decoder "h264"; then
    if [ -f /tmp/ffmpeg-codec-workdir/h264.mp4 ]; then
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/h264.mp4 \
            -f rawvideo -pix_fmt yuv420p \
            /tmp/ffmpeg-codec-workdir/h264_decoded.yuv 2>/dev/null \
            || fail "h264 decode failed"
        [ -s /tmp/ffmpeg-codec-workdir/h264_decoded.yuv ] || fail "h264 decoded output is empty"
    fi
else
    skip "h264-decode" "h264 decoder not available"
fi

# ---- Stage 2: MPEG-4 encode/decode ----
echo "FFMPEG_CODEC_STAGE mpeg4-encode"
if has_encoder "mpeg4"; then
    gen_raw_video /tmp/ffmpeg-codec-workdir/raw_mpeg4.yuv || fail "cannot generate raw video"
    ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_mpeg4.yuv \
        -c:v mpeg4 -q:v 10 \
        /tmp/ffmpeg-codec-workdir/mpeg4.avi 2>/dev/null \
        || fail "mpeg4 encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/mpeg4.avi ] || fail "mpeg4 output is empty"
else
    skip "mpeg4-encode" "mpeg4 encoder not available"
fi

echo "FFMPEG_CODEC_STAGE mpeg4-decode"
if has_decoder "mpeg4"; then
    if [ -f /tmp/ffmpeg-codec-workdir/mpeg4.avi ]; then
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/mpeg4.avi \
            -f rawvideo -pix_fmt yuv420p \
            /tmp/ffmpeg-codec-workdir/mpeg4_decoded.yuv 2>/dev/null \
            || fail "mpeg4 decode failed"
        [ -s /tmp/ffmpeg-codec-workdir/mpeg4_decoded.yuv ] || fail "mpeg4 decoded output is empty"
    fi
else
    skip "mpeg4-decode" "mpeg4 decoder not available"
fi

# ---- Stage 3: VP8 encode/decode ----
echo "FFMPEG_CODEC_STAGE vp8-encode"
if has_encoder "libvpx"; then
    gen_raw_video /tmp/ffmpeg-codec-workdir/raw_vp8.yuv 160 120 1 || fail "cannot generate raw video"
    ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_vp8.yuv \
        -c:v libvpx -quality realtime -cpu-used 4 \
        /tmp/ffmpeg-codec-workdir/vp8.webm 2>/dev/null \
        || fail "vp8 encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/vp8.webm ] || fail "vp8 output is empty"
else
    skip "vp8-encode" "libvpx not available"
fi

echo "FFMPEG_CODEC_STAGE vp8-decode"
if has_decoder "vp8"; then
    if [ -f /tmp/ffmpeg-codec-workdir/vp8.webm ]; then
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/vp8.webm \
            -f rawvideo -pix_fmt yuv420p \
            /tmp/ffmpeg-codec-workdir/vp8_decoded.yuv 2>/dev/null \
            || fail "vp8 decode failed"
        [ -s /tmp/ffmpeg-codec-workdir/vp8_decoded.yuv ] || fail "vp8 decoded output is empty"
    fi
else
    skip "vp8-decode" "vp8 decoder not available"
fi

# ---- Stage 4: VP9 encode/decode ----
echo "FFMPEG_CODEC_STAGE vp9-encode"
if has_encoder "libvpx-vp9"; then
    gen_raw_video /tmp/ffmpeg-codec-workdir/raw_vp9.yuv 160 120 1 || fail "cannot generate raw video"
    ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_vp9.yuv \
        -c:v libvpx-vp9 -quality realtime -cpu-used 4 \
        /tmp/ffmpeg-codec-workdir/vp9.webm 2>/dev/null \
        || fail "vp9 encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/vp9.webm ] || fail "vp9 output is empty"
else
    skip "vp9-encode" "libvpx-vp9 not available"
fi

# ---- Stage 5: MJPEG encode/decode ----
echo "FFMPEG_CODEC_STAGE mjpeg-encode"
if has_encoder "mjpeg"; then
    gen_raw_video /tmp/ffmpeg-codec-workdir/raw_mjpeg.yuv || fail "cannot generate raw video"
    ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_mjpeg.yuv \
        -c:v mjpeg -q:v 5 \
        /tmp/ffmpeg-codec-workdir/mjpeg.avi 2>/dev/null \
        || fail "mjpeg encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/mjpeg.avi ] || fail "mjpeg output is empty"
else
    skip "mjpeg-encode" "mjpeg encoder not available"
fi

echo "FFMPEG_CODEC_STAGE mjpeg-decode"
if has_decoder "mjpeg"; then
    if [ -f /tmp/ffmpeg-codec-workdir/mjpeg.avi ]; then
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/mjpeg.avi \
            -f rawvideo -pix_fmt yuv420p \
            /tmp/ffmpeg-codec-workdir/mjpeg_decoded.yuv 2>/dev/null \
            || fail "mjpeg decode failed"
        [ -s /tmp/ffmpeg-codec-workdir/mjpeg_decoded.yuv ] || fail "mjpeg decoded output is empty"
    fi
else
    skip "mjpeg-decode" "mjpeg decoder not available"
fi

# ---- Stage 6: Raw video (rawvideo) ----
echo "FFMPEG_CODEC_STAGE rawvideo"
gen_raw_video /tmp/ffmpeg-codec-workdir/raw_test.yuv || fail "cannot generate raw video"
ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_test.yuv \
    -c:v rawvideo -pix_fmt yuv420p \
    /tmp/ffmpeg-codec-workdir/raw_out.yuv 2>/dev/null \
    || fail "rawvideo copy failed"
[ -s /tmp/ffmpeg-codec-workdir/raw_out.yuv ] || fail "rawvideo output is empty"

# ===== AUDIO CODECS =====

# ---- Stage 7: MP3 (libmp3lame) encode/decode ----
echo "FFMPEG_CODEC_STAGE mp3-encode"
if has_encoder "libmp3lame"; then
    gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_audio.wav 2 || fail "cannot generate raw audio"
    ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_audio.wav \
        -c:a libmp3lame -b:a 128k \
        /tmp/ffmpeg-codec-workdir/audio.mp3 2>/dev/null \
        || fail "mp3 encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/audio.mp3 ] || fail "mp3 output is empty"
else
    skip "mp3-encode" "libmp3lame not available"
fi

echo "FFMPEG_CODEC_STAGE mp3-decode"
if has_decoder "mp3"; then
    if [ -f /tmp/ffmpeg-codec-workdir/audio.mp3 ]; then
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/audio.mp3 \
            -c:a pcm_s16le \
            /tmp/ffmpeg-codec-workdir/mp3_decoded.wav 2>/dev/null \
            || fail "mp3 decode failed"
        [ -s /tmp/ffmpeg-codec-workdir/mp3_decoded.wav ] || fail "mp3 decoded output is empty"
    fi
else
    skip "mp3-decode" "mp3 decoder not available"
fi

# ---- Stage 8: AAC encode/decode ----
echo "FFMPEG_CODEC_STAGE aac-encode"
if has_encoder "aac"; then
    gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_aac.wav 2 || fail "cannot generate raw audio"
    ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_aac.wav \
        -c:a aac -b:a 128k \
        /tmp/ffmpeg-codec-workdir/audio.aac 2>/dev/null \
        || fail "aac encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/audio.aac ] || fail "aac output is empty"
else
    skip "aac-encode" "aac encoder not available"
fi

echo "FFMPEG_CODEC_STAGE aac-decode"
if has_decoder "aac"; then
    if [ -f /tmp/ffmpeg-codec-workdir/audio.aac ]; then
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/audio.aac \
            -c:a pcm_s16le \
            /tmp/ffmpeg-codec-workdir/aac_decoded.wav 2>/dev/null \
            || fail "aac decode failed"
        [ -s /tmp/ffmpeg-codec-workdir/aac_decoded.wav ] || fail "aac decoded output is empty"
    fi
else
    skip "aac-decode" "aac decoder not available"
fi

# ---- Stage 9: Vorbis encode/decode ----
echo "FFMPEG_CODEC_STAGE vorbis-encode"
if has_encoder "libvorbis"; then
    gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_vorbis.wav 2 || fail "cannot generate raw audio"
    ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_vorbis.wav \
        -c:a libvorbis -b:a 128k \
        /tmp/ffmpeg-codec-workdir/audio.ogg 2>/dev/null \
        || fail "vorbis encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/audio.ogg ] || fail "vorbis output is empty"
else
    skip "vorbis-encode" "libvorbis not available"
fi

# ---- Stage 10: Opus encode/decode ----
echo "FFMPEG_CODEC_STAGE opus-encode"
if has_encoder "libopus"; then
    gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_opus.wav 2 || fail "cannot generate raw audio"
    ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_opus.wav \
        -c:a libopus -b:a 64k \
        /tmp/ffmpeg-codec-workdir/audio.opus 2>/dev/null \
        || fail "opus encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/audio.opus ] || fail "opus output is empty"
else
    skip "opus-encode" "libopus not available"
fi

echo "FFMPEG_CODEC_STAGE opus-decode"
if has_decoder "opus"; then
    if [ -f /tmp/ffmpeg-codec-workdir/audio.opus ]; then
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/audio.opus \
            -c:a pcm_s16le \
            /tmp/ffmpeg-codec-workdir/opus_decoded.wav 2>/dev/null \
            || fail "opus decode failed"
        [ -s /tmp/ffmpeg-codec-workdir/opus_decoded.wav ] || fail "opus decoded output is empty"
    fi
else
    skip "opus-decode" "opus decoder not available"
fi

# ---- Stage 11: FLAC encode/decode ----
echo "FFMPEG_CODEC_STAGE flac-encode"
if has_encoder "flac"; then
    gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_flac.wav 2 || fail "cannot generate raw audio"
    ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_flac.wav \
        -c:a flac \
        /tmp/ffmpeg-codec-workdir/audio.flac 2>/dev/null \
        || fail "flac encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/audio.flac ] || fail "flac output is empty"
else
    skip "flac-encode" "flac encoder not available"
fi

echo "FFMPEG_CODEC_STAGE flac-decode"
if has_decoder "flac"; then
    if [ -f /tmp/ffmpeg-codec-workdir/audio.flac ]; then
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/audio.flac \
            -c:a pcm_s16le \
            /tmp/ffmpeg-codec-workdir/flac_decoded.wav 2>/dev/null \
            || fail "flac decode failed"
        [ -s /tmp/ffmpeg-codec-workdir/flac_decoded.wav ] || fail "flac decoded output is empty"
    fi
else
    skip "flac-decode" "flac decoder not available"
fi

# ===== CONTAINER FORMATS =====

# ---- Stage 12: MKV container ----
echo "FFMPEG_CODEC_STAGE mkv-container"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mkv" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mkv" \
        -c copy \
        /tmp/ffmpeg-codec-workdir/remuxed.mkv 2>/dev/null \
        || fail "mkv remux failed"
    [ -s /tmp/ffmpeg-codec-workdir/remuxed.mkv ] || fail "mkv remuxed output is empty"
else
    skip "mkv-container" "no mkv test media"
fi

# ---- Stage 13: AVI container ----
echo "FFMPEG_CODEC_STAGE avi-container"
if [ -f "$TEST_MEDIA_DIR/test_160x120.avi" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.avi" \
        -c copy \
        /tmp/ffmpeg-codec-workdir/remuxed.avi 2>/dev/null \
        || fail "avi remux failed"
    [ -s /tmp/ffmpeg-codec-workdir/remuxed.avi ] || fail "avi remuxed output is empty"
else
    skip "avi-container" "no avi test media"
fi

# ---- Stage 14: WebM container ----
echo "FFMPEG_CODEC_STAGE webm-container"
if has_encoder "libvpx"; then
    gen_raw_video /tmp/ffmpeg-codec-workdir/raw_webm.yuv 160 120 1 || fail "cannot generate raw video"
    ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_webm.yuv \
        -c:v libvpx -quality realtime -cpu-used 4 \
        /tmp/ffmpeg-codec-workdir/test.webm 2>/dev/null \
        || fail "webm encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/test.webm ] || fail "webm output is empty"
    # Re-mux
    ffmpeg -y -i /tmp/ffmpeg-codec-workdir/test.webm \
        -c copy \
        /tmp/ffmpeg-codec-workdir/remuxed.webm 2>/dev/null \
        || fail "webm remux failed"
else
    skip "webm-container" "libvpx not available"
fi

# ---- Stage 15: Cross-container transcoding (MP4 -> WebM -> MKV) ----
echo "FFMPEG_CODEC_STAGE cross-container"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ] && has_encoder "libvpx"; then
    # MP4 -> WebM
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c:v libvpx -quality realtime -cpu-used 4 \
        /tmp/ffmpeg-codec-workdir/cross.webm 2>/dev/null \
        || fail "cross-container mp4->webm failed"
    # WebM -> MKV
    ffmpeg -y -i /tmp/ffmpeg-codec-workdir/cross.webm \
        -c copy \
        /tmp/ffmpeg-codec-workdir/cross.mkv 2>/dev/null \
        || fail "cross-container webm->mkv failed"
    [ -s /tmp/ffmpeg-codec-workdir/cross.mkv ] || fail "cross-container output is empty"
else
    skip "cross-container" "test media or libvpx not available"
fi

# ---- Stage 16: H.265 (libx265) encode/decode ----
echo "FFMPEG_CODEC_STAGE h265-encode"
if has_encoder "libx265"; then
    gen_raw_video /tmp/ffmpeg-codec-workdir/raw_h265.yuv 160 120 1 || fail "cannot generate raw video"
    ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 160x120 -i /tmp/ffmpeg-codec-workdir/raw_h265.yuv \
        -c:v libx265 -preset ultrafast -pix_fmt yuv420p \
        /tmp/ffmpeg-codec-workdir/h265.mp4 2>/dev/null \
        || fail "h265 encode failed"
    [ -s /tmp/ffmpeg-codec-workdir/h265.mp4 ] || fail "h265 output is empty"
else
    skip "h265-encode" "libx265 not available"
fi

echo "FFMPEG_CODEC_STAGE h265-decode"
if has_decoder "hevc"; then
    if [ -f /tmp/ffmpeg-codec-workdir/h265.mp4 ]; then
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/h265.mp4 \
            -f rawvideo -pix_fmt yuv420p \
            /tmp/ffmpeg-codec-workdir/h265_decoded.yuv 2>/dev/null \
            || fail "h265 decode failed"
        [ -s /tmp/ffmpeg-codec-workdir/h265_decoded.yuv ] || fail "h265 decoded output is empty"
    fi
else
    skip "h265-decode" "hevc decoder not available"
fi

# ---- Stage 17: Audio sample format conversion (s16 -> f32) ----
echo "FFMPEG_CODEC_STAGE sample-fmt-convert"
gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_s16.wav 2 || fail "cannot generate raw audio"
ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_s16.wav \
    -c:a pcm_f32le \
    /tmp/ffmpeg-codec-workdir/output_f32.wav 2>/dev/null \
    || fail "sample format conversion failed"
[ -s /tmp/ffmpeg-codec-workdir/output_f32.wav ] || fail "f32 output is empty"

# ---- Stage 18: Audio bit rate ladder (same source, different bitrates) ----
echo "FFMPEG_CODEC_STAGE bitrate-ladder"
if has_encoder "libmp3lame"; then
    gen_raw_audio /tmp/ffmpeg-codec-workdir/raw_bl.wav 2 || fail "cannot generate raw audio"
    for br in 64 128 192 256; do
        ffmpeg -y -i /tmp/ffmpeg-codec-workdir/raw_bl.wav \
            -c:a libmp3lame -b:a "${br}k" \
            "/tmp/ffmpeg-codec-workdir/audio_${br}k.mp3" 2>/dev/null \
            || fail "mp3 encode at ${br}k failed"
        [ -s "/tmp/ffmpeg-codec-workdir/audio_${br}k.mp3" ] || fail "mp3 at ${br}k is empty"
    done
else
    skip "bitrate-ladder" "libmp3lame not available"
fi

# ---- Stage 19: Mux separate video + audio into one file ----
echo "FFMPEG_CODEC_STAGE mux-av"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ] && [ -f "$TEST_MEDIA_DIR/test_audio.mp3" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" -i "$TEST_MEDIA_DIR/test_audio.mp3" \
        -c:v copy -c:a aac -shortest \
        /tmp/ffmpeg-codec-workdir/muxed_av.mp4 2>/dev/null \
        || fail "mux video+audio failed"
    [ -s /tmp/ffmpeg-codec-workdir/muxed_av.mp4 ] || fail "muxed output is empty"
    # Verify both streams present
    ffprobe -v quiet -show_entries stream=codec_type \
        /tmp/ffmpeg-codec-workdir/muxed_av.mp4 2>&1 | grep -q "video" \
        || fail "video stream missing in muxed file"
    ffprobe -v quiet -show_entries stream=codec_type \
        /tmp/ffmpeg-codec-workdir/muxed_av.mp4 2>&1 | grep -q "audio" \
        || fail "audio stream missing in muxed file"
else
    skip "mux-av" "test media not available"
fi

# ---- Stage 20: Video resolution ladder ----
echo "FFMPEG_CODEC_STAGE resolution-ladder"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ] && has_encoder "libx264"; then
    for res in "80:60" "160:120"; do
        ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
            -vf "scale=$res" -c:v libx264 -preset ultrafast -frames:v 3 \
            "/tmp/ffmpeg-codec-workdir/res_$(echo $res | tr ':' 'x').mp4" 2>/dev/null \
            || fail "resolution ladder at $res failed"
        [ -s "/tmp/ffmpeg-codec-workdir/res_$(echo $res | tr ':' 'x').mp4" ] || fail "output at $res is empty"
    done
else
    skip "resolution-ladder" "test media or libx264 not available"
fi

test_done=1
trap - EXIT
cleanup

echo "FFMPEG_CODEC_TEST_PASSED"
