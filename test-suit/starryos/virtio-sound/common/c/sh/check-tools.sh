#!/bin/sh
set -u

tmp="/tmp/virtio-sound-tools.$$"
trap 'rm -f "$tmp".*' EXIT HUP INT TERM

fail() {
    echo "VIRTIO_SOUND_TOOLS_FAILED: $*" >&2
    exit 1
}

check_version() {
    tool="$1"

    "$tool" --version >"$tmp.$tool.version" 2>&1 ||
        fail "$tool --version exited non-zero"
    grep -Eq 'version 1\.2\.13([^0-9]|$)' "$tmp.$tool.version" ||
        fail "$tool is not ALSA 1.2.13: $(cat "$tmp.$tool.version")"
}

check_list() {
    tool="$1"
    direction="$2"

    "$tool" -l >"$tmp.$tool.list" 2>&1 ||
        fail "$tool -l exited non-zero"
    grep -Eq '^card[[:space:]]+0:.*device[[:space:]]+0:' "$tmp.$tool.list" ||
        fail "$tool -l has no card 0 device 0 $direction: $(cat "$tmp.$tool.list")"
}

for node in /dev/snd/controlC0 /dev/snd/pcmC0D0p /dev/snd/pcmC0D0c; do
    [ -e "$node" ] || fail "missing ALSA node $node"
done

check_version aplay
check_version arecord
check_list aplay playback
check_list arecord capture
