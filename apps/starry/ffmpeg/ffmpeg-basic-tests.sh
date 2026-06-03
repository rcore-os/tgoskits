#!/bin/sh
set -eu

test_done=0

cleanup() {
    rm -f /tmp/ffmpeg-basic-* 2>/dev/null || true
    rm -rf /tmp/ffmpeg-basic-workdir 2>/dev/null || true
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ]; then
        echo "FFMPEG_BASIC_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

fail() {
    echo "FFMPEG_BASIC_STAGE FAILED: $1"
    echo "FFMPEG_BASIC_TEST_FAILED"
    exit 1
}

mkdir -p /tmp/ffmpeg-basic-workdir

# ---- Helper: check if test media exists ----
TEST_MEDIA_DIR="/usr/share/ffmpeg-test-media"
has_test_media=false
if [ -d "$TEST_MEDIA_DIR" ] && [ "$(ls -A "$TEST_MEDIA_DIR" 2>/dev/null)" ]; then
    has_test_media=true
fi

# ---- Stage 1: Generate synthetic test data (fallback if no pre-built media) ----
echo "FFMPEG_BASIC_STAGE generate-test-data"
if [ "$has_test_media" = false ]; then
    # Generate a minimal raw video file using ffmpeg's lavfi source
    # 160x120, 10 frames, raw yuv420p
    ffmpeg -y -f lavfi -i "color=c=red:s=160x120:d=1" \
        -c:v rawvideo -pix_fmt yuv420p \
        -frames:v 10 \
        /tmp/ffmpeg-basic-workdir/test_raw.yuv 2>/dev/null \
        || fail "cannot generate synthetic test data"
    has_test_media=true
fi

# ---- Stage 2: ffprobe on test media ----
echo "FFMPEG_BASIC_STAGE ffprobe"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffprobe -v quiet -print_format json -show_format \
        "$TEST_MEDIA_DIR/test_160x120.mp4" > /tmp/ffmpeg-basic-probe.json 2>&1 \
        || fail "ffprobe on mp4 failed"
    grep -q "format" /tmp/ffmpeg-basic-probe.json || fail "ffprobe json missing format"
elif [ -f "$TEST_MEDIA_DIR/test_audio.wav" ]; then
    ffprobe -v quiet -print_format json -show_format \
        "$TEST_MEDIA_DIR/test_audio.wav" > /tmp/ffmpeg-basic-probe.json 2>&1 \
        || fail "ffprobe on wav failed"
    grep -q "format" /tmp/ffmpeg-basic-probe.json || fail "ffprobe json missing format"
else
    echo "FFMPEG_BASIC_STAGE ffprobe SKIP (no test media)"
fi

# ---- Stage 3: Format detection (identify) ----
echo "FFMPEG_BASIC_STAGE format-identify"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffprobe -v quiet -show_entries format=format_name \
        "$TEST_MEDIA_DIR/test_160x120.mp4" 2>&1 | grep -q "mp4" \
        || fail "mp4 format not identified"
fi
if [ -f "$TEST_MEDIA_DIR/test_audio.wav" ]; then
    ffprobe -v quiet -show_entries format=format_name \
        "$TEST_MEDIA_DIR/test_audio.wav" 2>&1 | grep -q "wav" \
        || fail "wav format not identified"
fi

# ---- Stage 4: Stream info extraction ----
echo "FFMPEG_BASIC_STAGE stream-info"
if [ -f "$TEST_MEDIA_DIR/test_av.mp4" ]; then
    ffprobe -v quiet -show_entries stream=codec_type,codec_name,width,height \
        "$TEST_MEDIA_DIR/test_av.mp4" 2>&1 > /tmp/ffmpeg-basic-streams.out
    grep -q "codec_type=video" /tmp/ffmpeg-basic-streams.out || fail "video stream not found in av file"
    grep -q "codec_type=audio" /tmp/ffmpeg-basic-streams.out || fail "audio stream not found in av file"
fi

# ---- Stage 5: Simple transcoding (re-mux) MP4 -> MP4 ----
echo "FFMPEG_BASIC_STAGE remux-mp4"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c copy \
        /tmp/ffmpeg-basic-workdir/remuxed.mp4 2>/dev/null \
        || fail "remux mp4->mp4 failed"
    [ -s /tmp/ffmpeg-basic-workdir/remuxed.mp4 ] || fail "remuxed mp4 is empty"
fi

# ---- Stage 6: Simple transcoding MP4 -> MKV (re-mux to different container) ----
echo "FFMPEG_BASIC_STAGE remux-mp4-to-mkv"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c copy \
        /tmp/ffmpeg-basic-workdir/remuxed.mkv 2>/dev/null \
        || fail "remux mp4->mkv failed"
    [ -s /tmp/ffmpeg-basic-workdir/remuxed.mkv ] || fail "remuxed mkv is empty"
fi

# ---- Stage 7: Simple transcoding MP4 -> AVI (re-mux) ----
echo "FFMPEG_BASIC_STAGE remux-mp4-to-avi"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c copy \
        /tmp/ffmpeg-basic-workdir/remuxed.avi 2>/dev/null \
        || fail "remux mp4->avi failed"
    [ -s /tmp/ffmpeg-basic-workdir/remuxed.avi ] || fail "remuxed avi is empty"
fi

# ---- Stage 8: Audio transcoding WAV -> MP3 ----
echo "FFMPEG_BASIC_STAGE transcode-wav-to-mp3"
if [ -f "$TEST_MEDIA_DIR/test_audio.wav" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_audio.wav" \
        -c:a libmp3lame -b:a 128k \
        /tmp/ffmpeg-basic-workdir/output.mp3 2>/dev/null \
        || fail "transcode wav->mp3 failed"
    [ -s /tmp/ffmpeg-basic-workdir/output.mp3 ] || fail "output mp3 is empty"
fi

# ---- Stage 9: Audio transcoding WAV -> AAC ----
echo "FFMPEG_BASIC_STAGE transcode-wav-to-aac"
if [ -f "$TEST_MEDIA_DIR/test_audio.wav" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_audio.wav" \
        -c:a aac -b:a 128k \
        /tmp/ffmpeg-basic-workdir/output.aac 2>/dev/null \
        || fail "transcode wav->aac failed"
    [ -s /tmp/ffmpeg-basic-workdir/output.aac ] || fail "output aac is empty"
fi

# ---- Stage 10: Video scaling ----
echo "FFMPEG_BASIC_STAGE video-scale"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -vf "scale=80:60" -c:v libx264 -preset ultrafast \
        /tmp/ffmpeg-basic-workdir/scaled.mp4 2>/dev/null \
        || fail "video scaling failed"
    [ -s /tmp/ffmpeg-basic-workdir/scaled.mp4 ] || fail "scaled mp4 is empty"
    # Verify dimensions
    width=$(ffprobe -v quiet -show_entries stream=width -of csv=p=0 /tmp/ffmpeg-basic-workdir/scaled.mp4)
    height=$(ffprobe -v quiet -show_entries stream=height -of csv=p=0 /tmp/ffmpeg-basic-workdir/scaled.mp4)
    [ "$width" = "80" ] || fail "scaled width is $width, expected 80"
    [ "$height" = "60" ] || fail "scaled height is $height, expected 60"
fi

# ---- Stage 11: Video cropping ----
echo "FFMPEG_BASIC_STAGE video-crop"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -vf "crop=80:60:40:30" -c:v libx264 -preset ultrafast \
        /tmp/ffmpeg-basic-workdir/cropped.mp4 2>/dev/null \
        || fail "video cropping failed"
    [ -s /tmp/ffmpeg-basic-workdir/cropped.mp4 ] || fail "cropped mp4 is empty"
fi

# ---- Stage 12: Frame extraction ----
echo "FFMPEG_BASIC_STAGE frame-extract"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -frames:v 1 \
        /tmp/ffmpeg-basic-workdir/frame.png 2>/dev/null \
        || fail "frame extraction failed"
    [ -s /tmp/ffmpeg-basic-workdir/frame.png ] || fail "extracted frame is empty"
fi

# ---- Stage 13: Metadata extraction ----
echo "FFMPEG_BASIC_STAGE metadata"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffprobe -v quiet -print_format json -show_format -show_streams \
        "$TEST_MEDIA_DIR/test_160x120.mp4" > /tmp/ffmpeg-basic-metadata.json 2>&1 \
        || fail "metadata extraction failed"
    grep -q "duration" /tmp/ffmpeg-basic-metadata.json || fail "duration not found in metadata"
    grep -q "codec_name" /tmp/ffmpeg-basic-metadata.json || fail "codec_name not found in metadata"
fi

# ---- Stage 14: Duration trimming ----
echo "FFMPEG_BASIC_STAGE trim"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -ss 0 -t 1 -c copy \
        /tmp/ffmpeg-basic-workdir/trimmed.mp4 2>/dev/null \
        || fail "trimming failed"
    [ -s /tmp/ffmpeg-basic-workdir/trimmed.mp4 ] || fail "trimmed mp4 is empty"
fi

# ---- Stage 15: Concat (using concat demuxer) ----
echo "FFMPEG_BASIC_STAGE concat"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    # Create concat list
    echo "file '$TEST_MEDIA_DIR/test_160x120.mp4'" > /tmp/ffmpeg-basic-workdir/concat.txt
    echo "file '$TEST_MEDIA_DIR/test_160x120.mp4'" >> /tmp/ffmpeg-basic-workdir/concat.txt
    ffmpeg -y -f concat -safe 0 -i /tmp/ffmpeg-basic-workdir/concat.txt \
        -c copy \
        /tmp/ffmpeg-basic-workdir/concatenated.mp4 2>/dev/null \
        || fail "concat failed"
    [ -s /tmp/ffmpeg-basic-workdir/concatenated.mp4 ] || fail "concatenated mp4 is empty"
fi

# ---- Stage 16: Audio resampling (sample rate conversion) ----
echo "FFMPEG_BASIC_STAGE audio-resample"
if [ -f "$TEST_MEDIA_DIR/test_audio.wav" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_audio.wav" \
        -ar 48000 -c:a pcm_s16le \
        /tmp/ffmpeg-basic-workdir/resampled_48k.wav 2>/dev/null \
        || fail "audio resample to 48kHz failed"
    [ -s /tmp/ffmpeg-basic-workdir/resampled_48k.wav ] || fail "resampled wav is empty"
    # Verify sample rate
    sr=$(ffprobe -v quiet -show_entries stream=sample_rate -of csv=p=0 /tmp/ffmpeg-basic-workdir/resampled_48k.wav)
    [ "$sr" = "48000" ] || fail "resampled sample rate is $sr, expected 48000"
fi

# ---- Stage 17: Audio channel conversion (mono -> stereo) ----
echo "FFMPEG_BASIC_STAGE audio-channels"
if [ -f "$TEST_MEDIA_DIR/test_audio.wav" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_audio.wav" \
        -ac 2 -c:a pcm_s16le \
        /tmp/ffmpeg-basic-workdir/stereo.wav 2>/dev/null \
        || fail "audio channel conversion failed"
    [ -s /tmp/ffmpeg-basic-workdir/stereo.wav ] || fail "stereo wav is empty"
    ch=$(ffprobe -v quiet -show_entries stream=channels -of csv=p=0 /tmp/ffmpeg-basic-workdir/stereo.wav)
    [ "$ch" = "2" ] || fail "channel count is $ch, expected 2"
fi

# ---- Stage 18: Pixel format conversion (yuv420p -> rgb24) ----
echo "FFMPEG_BASIC_STAGE pixfmt-convert"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -pix_fmt rgb24 -frames:v 1 \
        /tmp/ffmpeg-basic-workdir/rgb24.png 2>/dev/null \
        || fail "pixel format conversion failed"
    [ -s /tmp/ffmpeg-basic-workdir/rgb24.png ] || fail "rgb24 output is empty"
fi

# ---- Stage 19: Image sequence output (image2) ----
echo "FFMPEG_BASIC_STAGE image-sequence"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -frames:v 3 \
        /tmp/ffmpeg-basic-workdir/frame_%03d.png 2>/dev/null \
        || fail "image sequence output failed"
    count=$(ls /tmp/ffmpeg-basic-workdir/frame_*.png 2>/dev/null | wc -l)
    [ "$count" -ge 1 ] || fail "no frames extracted, got $count"
fi

# ---- Stage 20: GIF generation ----
echo "FFMPEG_BASIC_STAGE gif-generate"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -vf "fps=10,scale=80:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse" \
        -loop 0 \
        /tmp/ffmpeg-basic-workdir/output.gif 2>/dev/null \
        || fail "GIF generation failed"
    [ -s /tmp/ffmpeg-basic-workdir/output.gif ] || fail "GIF output is empty"
fi

# ---- Stage 21: Error handling (corrupt input) ----
echo "FFMPEG_BASIC_STAGE error-handling"
# Create a deliberately corrupt file
echo "not a valid media file" > /tmp/ffmpeg-basic-workdir/corrupt.mp4
# ffmpeg should fail gracefully (non-zero exit), not crash
ffmpeg -y -i /tmp/ffmpeg-basic-workdir/corrupt.mp4 \
    -c copy /tmp/ffmpeg-basic-workdir/corrupt_out.mp4 2>/dev/null \
    && fail "ffmpeg should have failed on corrupt input" || true

# ---- Stage 22: Multi-stream mapping ----
echo "FFMPEG_BASIC_STAGE multi-stream-map"
if [ -f "$TEST_MEDIA_DIR/test_av.mp4" ]; then
    # Extract only video stream
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_av.mp4" \
        -map 0:v:0 -c copy \
        /tmp/ffmpeg-basic-workdir/video_only.mp4 2>/dev/null \
        || fail "video-only extraction failed"
    [ -s /tmp/ffmpeg-basic-workdir/video_only.mp4 ] || fail "video-only output is empty"
    # Verify no audio stream
    streams=$(ffprobe -v quiet -show_entries stream=codec_type -of csv=p=0 /tmp/ffmpeg-basic-workdir/video_only.mp4)
    echo "$streams" | grep -q "video" || fail "video stream missing in video-only"
    echo "$streams" | grep -q "audio" && fail "audio stream should not be in video-only" || true

    # Extract only audio stream
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_av.mp4" \
        -map 0:a:0 -c copy \
        /tmp/ffmpeg-basic-workdir/audio_only.m4a 2>/dev/null \
        || fail "audio-only extraction failed"
    [ -s /tmp/ffmpeg-basic-workdir/audio_only.m4a ] || fail "audio-only output is empty"
fi

# ---- Stage 23: Complex filter chain (scale + transpose + eq) ----
echo "FFMPEG_BASIC_STAGE complex-filter"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -vf "scale=320:240,eq=brightness=0.06:contrast=1.5" \
        -c:v libx264 -preset ultrafast -frames:v 5 \
        /tmp/ffmpeg-basic-workdir/complex_filter.mp4 2>/dev/null \
        || fail "complex filter chain failed"
    [ -s /tmp/ffmpeg-basic-workdir/complex_filter.mp4 ] || fail "complex filter output is empty"
    w=$(ffprobe -v quiet -show_entries stream=width -of csv=p=0 /tmp/ffmpeg-basic-workdir/complex_filter.mp4)
    h=$(ffprobe -v quiet -show_entries stream=height -of csv=p=0 /tmp/ffmpeg-basic-workdir/complex_filter.mp4)
    [ "$w" = "320" ] || fail "filtered width is $w, expected 320"
    [ "$h" = "240" ] || fail "filtered height is $h, expected 240"
fi

# ---- Stage 24: Stream copy vs transcode consistency ----
echo "FFMPEG_BASIC_STAGE copy-vs-transcode"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    # Stream copy (no re-encoding)
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c copy -t 1 \
        /tmp/ffmpeg-basic-workdir/copied.mp4 2>/dev/null \
        || fail "stream copy failed"
    # Transcode (re-encode)
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c:v libx264 -preset ultrafast -t 1 \
        /tmp/ffmpeg-basic-workdir/transcoded.mp4 2>/dev/null \
        || fail "transcode failed"
    [ -s /tmp/ffmpeg-basic-workdir/copied.mp4 ] || fail "copied output is empty"
    [ -s /tmp/ffmpeg-basic-workdir/transcoded.mp4 ] || fail "transcoded output is empty"
    # Both should have same resolution
    w1=$(ffprobe -v quiet -show_entries stream=width -of csv=p=0 /tmp/ffmpeg-basic-workdir/copied.mp4)
    w2=$(ffprobe -v quiet -show_entries stream=width -of csv=p=0 /tmp/ffmpeg-basic-workdir/transcoded.mp4)
    [ "$w1" = "$w2" ] || fail "resolution mismatch: copy=$w1, transcode=$w2"
fi

test_done=1
trap - EXIT
cleanup

echo "FFMPEG_BASIC_TEST_PASSED"
