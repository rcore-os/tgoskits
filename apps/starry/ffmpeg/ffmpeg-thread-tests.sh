#!/bin/sh
set -eu

test_done=0

cleanup() {
    rm -rf /tmp/ffmpeg-thread-* 2>/dev/null || true
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ]; then
        echo "FFMPEG_THREAD_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

fail() {
    echo "FFMPEG_THREAD_STAGE FAILED: $1"
    echo "FFMPEG_THREAD_TEST_FAILED"
    exit 1
}

mkdir -p /tmp/ffmpeg-thread-workdir

# ---- Generate test media if not pre-built ----
. /usr/bin/ffmpeg-ensure-media.sh
TEST_MEDIA_DIR="$FFMPEG_TEST_MEDIA_DIR"

# ---- Stage 1: Single-thread baseline (for comparison) ----
echo "FFMPEG_THREAD_STAGE single-thread"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -threads 1 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    /tmp/ffmpeg-thread-workdir/out_1thread.mp4 2>/dev/null \
    || fail "single-thread encode failed"
[ -s /tmp/ffmpeg-thread-workdir/out_1thread.mp4 ] || fail "single-thread output is empty"

# ---- Stage 2: Two threads ----
echo "FFMPEG_THREAD_STAGE two-threads"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -threads 2 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    /tmp/ffmpeg-thread-workdir/out_2threads.mp4 2>/dev/null \
    || fail "two-thread encode failed"
[ -s /tmp/ffmpeg-thread-workdir/out_2threads.mp4 ] || fail "two-thread output is empty"

# ---- Stage 3: Four threads ----
echo "FFMPEG_THREAD_STAGE four-threads"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -threads 4 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    /tmp/ffmpeg-thread-workdir/out_4threads.mp4 2>/dev/null \
    || fail "four-thread encode failed"
[ -s /tmp/ffmpeg-thread-workdir/out_4threads.mp4 ] || fail "four-thread output is empty"

# ---- Stage 4: Verify output consistency (single vs multi-thread) ----
echo "FFMPEG_THREAD_STAGE verify-consistency"
# Both outputs should be valid MP4 files with same resolution
width1=$(ffprobe -v quiet -show_entries stream=width -of csv=p=0 /tmp/ffmpeg-thread-workdir/out_1thread.mp4)
height1=$(ffprobe -v quiet -show_entries stream=height -of csv=p=0 /tmp/ffmpeg-thread-workdir/out_1thread.mp4)
width4=$(ffprobe -v quiet -show_entries stream=width -of csv=p=0 /tmp/ffmpeg-thread-workdir/out_4threads.mp4)
height4=$(ffprobe -v quiet -show_entries stream=height -of csv=p=0 /tmp/ffmpeg-thread-workdir/out_4threads.mp4)
[ "$width1" = "$width4" ] || fail "resolution mismatch: 1-thread=${width1}x${height1}, 4-thread=${width4}x${height4}"
[ "$height1" = "$height4" ] || fail "resolution mismatch: 1-thread=${width1}x${height1}, 4-thread=${width4}x${height4}"

# ---- Stage 5: Multi-threaded decoding ----
echo "FFMPEG_THREAD_STAGE multi-thread-decode"
ffmpeg -y -threads 4 -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -f rawvideo -pix_fmt yuv420p \
    /tmp/ffmpeg-thread-workdir/decoded_4thread.yuv 2>/dev/null \
    || fail "multi-thread decode failed"
[ -s /tmp/ffmpeg-thread-workdir/decoded_4thread.yuv ] || fail "multi-thread decoded output is empty"

# ---- Stage 6: Simultaneous encode + decode (pipeline) ----
echo "FFMPEG_THREAD_STAGE pipeline"
ffmpeg -y -threads 2 -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -threads 2 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -f mp4 /tmp/ffmpeg-thread-workdir/pipeline.mp4 2>/dev/null \
    || fail "pipeline encode/decode failed"
[ -s /tmp/ffmpeg-thread-workdir/pipeline.mp4 ] || fail "pipeline output is empty"

# ---- Stage 7: Audio encoding with threads ----
echo "FFMPEG_THREAD_STAGE audio-threads"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_audio.wav" \
    -threads 2 -c:a libmp3lame -b:a 128k \
    /tmp/ffmpeg-thread-workdir/audio_2threads.mp3 2>/dev/null \
    || fail "threaded audio encode failed"
[ -s /tmp/ffmpeg-thread-workdir/audio_2threads.mp3 ] || fail "threaded audio output is empty"

# ---- Stage 8: A/V sync with multiple threads ----
echo "FFMPEG_THREAD_STAGE av-sync-threads"
ffmpeg -y -threads 4 -i "$TEST_MEDIA_DIR/test_av.mp4" \
    -threads 4 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -b:a 64k \
    /tmp/ffmpeg-thread-workdir/av_sync.mp4 2>/dev/null \
    || fail "A/V sync with threads failed"
[ -s /tmp/ffmpeg-thread-workdir/av_sync.mp4 ] || fail "A/V sync output is empty"
# Verify both streams present
ffprobe -v quiet -show_entries stream=codec_type \
    /tmp/ffmpeg-thread-workdir/av_sync.mp4 2>&1 | grep -q "video" \
    || fail "video stream missing after threaded A/V encode"
ffprobe -v quiet -show_entries stream=codec_type \
    /tmp/ffmpeg-thread-workdir/av_sync.mp4 2>&1 | grep -q "audio" \
    || fail "audio stream missing after threaded A/V encode"

# ---- Stage 9: Multi-threaded filtering (scale + crop chain) ----
echo "FFMPEG_THREAD_STAGE threaded-filter"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -threads 4 -vf "scale=320:240,crop=160:120" \
    -c:v libx264 -preset ultrafast \
    /tmp/ffmpeg-thread-workdir/filtered.mp4 2>/dev/null \
    || fail "threaded filter chain failed"
[ -s /tmp/ffmpeg-thread-workdir/filtered.mp4 ] || fail "filtered output is empty"

# ---- Stage 10: Concurrent encode and decode (parallel pipelines) ----
echo "FFMPEG_THREAD_STAGE concurrent-pipelines"
# Run two independent encode operations in parallel
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -threads 2 -c:v libx264 -preset ultrafast \
    /tmp/ffmpeg-thread-workdir/pipe_a.mp4 2>/dev/null &
pid_a=$!
ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -threads 2 -c:v libx264 -preset ultrafast \
    /tmp/ffmpeg-thread-workdir/pipe_b.mp4 2>/dev/null &
pid_b=$!
wait "$pid_a" || fail "concurrent pipeline A failed"
wait "$pid_b" || fail "concurrent pipeline B failed"
[ -s /tmp/ffmpeg-thread-workdir/pipe_a.mp4 ] || fail "pipeline A output is empty"
[ -s /tmp/ffmpeg-thread-workdir/pipe_b.mp4 ] || fail "pipeline B output is empty"

# ---- Stage 11: Threaded decode to raw frames (memory intensive) ----
echo "FFMPEG_THREAD_STAGE threaded-decode-raw"
ffmpeg -y -threads 4 -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -f rawvideo -pix_fmt yuv420p \
    /tmp/ffmpeg-thread-workdir/decoded_raw.yuv 2>/dev/null \
    || fail "threaded raw decode failed"
[ -s /tmp/ffmpeg-thread-workdir/decoded_raw.yuv ] || fail "raw decoded output is empty"

# ---- Stage 12: Threaded audio resampling + encoding ----
echo "FFMPEG_THREAD_STAGE threaded-audio-resample"
ffmpeg -y -i "$TEST_MEDIA_DIR/test_audio.wav" \
    -threads 2 -ar 48000 -ac 2 -c:a libmp3lame -b:a 192k \
    /tmp/ffmpeg-thread-workdir/audio_resampled.mp3 2>/dev/null \
    || fail "threaded audio resample+encode failed"
[ -s /tmp/ffmpeg-thread-workdir/audio_resampled.mp3 ] || fail "threaded audio output is empty"

test_done=1
trap - EXIT
cleanup

echo "FFMPEG_THREAD_TEST_PASSED"
