#!/bin/sh
# ffmpeg-ensure-media.sh — Generate test media files inside QEMU guest.
# Source this script from other test scripts:
#   . /usr/bin/ffmpeg-ensure-media.sh
# After sourcing, $FFMPEG_TEST_MEDIA_DIR points to the media directory.

FFMPEG_TEST_MEDIA_DIR="/tmp/ffmpeg-test-media"

# Skip if already generated
if [ -d "$FFMPEG_TEST_MEDIA_DIR" ] && [ -f "$FFMPEG_TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    export FFMPEG_TEST_MEDIA_DIR
    return 0 2>/dev/null || exit 0
fi

mkdir -p "$FFMPEG_TEST_MEDIA_DIR"

echo "[ensure-media] generating test media in $FFMPEG_TEST_MEDIA_DIR ..."

# 1. H.264 MP4 video
ffmpeg -y -f lavfi -i "color=c=red:s=160x120:d=2" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    "$FFMPEG_TEST_MEDIA_DIR/test_160x120.mp4" 2>/dev/null \
    || { echo "[ensure-media] FAILED: test_160x120.mp4"; return 1; }

# 2. MP3 audio
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=2" \
    -c:a libmp3lame -b:a 128k \
    "$FFMPEG_TEST_MEDIA_DIR/test_audio.mp3" 2>/dev/null \
    || { echo "[ensure-media] FAILED: test_audio.mp3"; return 1; }

# 3. H.264 MKV container
ffmpeg -y -f lavfi -i "color=c=green:s=160x120:d=2" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    "$FFMPEG_TEST_MEDIA_DIR/test_160x120.mkv" 2>/dev/null \
    || { echo "[ensure-media] FAILED: test_160x120.mkv"; return 1; }

# 4. MPEG-4 AVI container
ffmpeg -y -f lavfi -i "color=c=yellow:s=160x120:d=2" \
    -c:v mpeg4 -q:v 10 \
    "$FFMPEG_TEST_MEDIA_DIR/test_160x120.avi" 2>/dev/null \
    || { echo "[ensure-media] FAILED: test_160x120.avi"; return 1; }

# 5. A/V muxed (H.264 video + AAC audio)
ffmpeg -y -f lavfi -i "color=c=blue:s=160x120:d=2" \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -b:a 64k \
    "$FFMPEG_TEST_MEDIA_DIR/test_av.mp4" 2>/dev/null \
    || { echo "[ensure-media] FAILED: test_av.mp4"; return 1; }

# 6. PCM WAV audio
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=2" \
    -c:a pcm_s16le \
    "$FFMPEG_TEST_MEDIA_DIR/test_audio.wav" 2>/dev/null \
    || { echo "[ensure-media] FAILED: test_audio.wav"; return 1; }

echo "[ensure-media] all 6 test media files generated"
export FFMPEG_TEST_MEDIA_DIR
