#!/bin/sh
set -eu

test_done=0

cleanup() {
    rm -f /tmp/ffmpeg-smoke-* 2>/dev/null || true
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ]; then
        echo "FFMPEG_SMOKE_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

fail() {
    echo "FFMPEG_SMOKE_STAGE FAILED: $1"
    echo "FFMPEG_SMOKE_TEST_FAILED"
    exit 1
}

echo "FFMPEG_SMOKE_STAGE version"
# Test ffmpeg version output
ffmpeg -version > /tmp/ffmpeg-smoke-version.out 2>&1 || fail "ffmpeg -version failed"
grep -q "ffmpeg version" /tmp/ffmpeg-smoke-version.out || fail "ffmpeg version string not found"

echo "FFMPEG_SMOKE_STAGE help"
# Test ffmpeg help output
ffmpeg -h > /tmp/ffmpeg-smoke-help.out 2>&1 || fail "ffmpeg -h failed"
grep -q "ffmpeg" /tmp/ffmpeg-smoke-help.out || fail "ffmpeg help string not found"

echo "FFMPEG_SMOKE_STAGE codecs"
# Test codec listing
ffmpeg -codecs > /tmp/ffmpeg-smoke-codecs.out 2>&1 || fail "ffmpeg -codecs failed"
grep -q "Codecs:" /tmp/ffmpeg-smoke-codecs.out || fail "Codecs: header not found"

echo "FFMPEG_SMOKE_STAGE formats"
# Test format listing
ffmpeg -formats > /tmp/ffmpeg-smoke-formats.out 2>&1 || fail "ffmpeg -formats failed"
grep -qE "Formats:|File formats:" /tmp/ffmpeg-smoke-formats.out || fail "Formats: header not found"

echo "FFMPEG_SMOKE_STAGE demuxers"
# Test demuxer listing
ffmpeg -demuxers > /tmp/ffmpeg-smoke-demuxers.out 2>&1 || fail "ffmpeg -demuxers failed"

echo "FFMPEG_SMOKE_STAGE muxers"
# Test muxer listing
ffmpeg -muxers > /tmp/ffmpeg-smoke-muxers.out 2>&1 || fail "ffmpeg -muxers failed"

echo "FFMPEG_SMOKE_STAGE protocols"
# Test protocol listing
ffmpeg -protocols > /tmp/ffmpeg-smoke-protocols.out 2>&1 || fail "ffmpeg -protocols failed"

echo "FFMPEG_SMOKE_STAGE filters"
# Test filter listing
ffmpeg -filters > /tmp/ffmpeg-smoke-filters.out 2>&1 || fail "ffmpeg -filters failed"

echo "FFMPEG_SMOKE_STAGE pix_fmts"
# Test pixel format listing
ffmpeg -pix_fmts > /tmp/ffmpeg-smoke-pixfmts.out 2>&1 || fail "ffmpeg -pix_fmts failed"

echo "FFMPEG_SMOKE_STAGE sample_fmts"
# Test sample format listing
ffmpeg -sample_fmts > /tmp/ffmpeg-smoke-samplefmts.out 2>&1 || fail "ffmpeg -sample_fmts failed"

echo "FFMPEG_SMOKE_STAGE bsfs"
# Test bitstream filter listing
ffmpeg -bsfs > /tmp/ffmpeg-smoke-bsfs.out 2>&1 || fail "ffmpeg -bsfs failed"

echo "FFMPEG_SMOKE_STAGE buildconf"
# Test build configuration
ffmpeg -buildconf > /tmp/ffmpeg-smoke-buildconf.out 2>&1 || fail "ffmpeg -buildconf failed"

test_done=1
trap - EXIT
cleanup

echo "FFMPEG_SMOKE_TEST_PASSED"
