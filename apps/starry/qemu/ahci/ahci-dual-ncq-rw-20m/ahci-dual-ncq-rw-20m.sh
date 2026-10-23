#!/bin/sh
# AHCI dual-disk NCQ read/write smoke: 20 MiB written and read back in parallel on
# both AHCI disks (port0 = the rootfs disk, port1 = the vfat data disk mounted at
# /partition), each copy verified with sha256sum.
#
# prebuild.sh stages this body at /usr/local/bin and qemu-x86_64.toml runs it with a
# single short line, so the guest's interactive shell never receives a multi-line
# command: injected multi-line text interleaves with the shell's own console output
# and can lose bytes before the shell reads it.

fail() {
    echo 'AHCI_DUAL_NCQ_RW_20M_FAILED'
    exit 1
}

attempt=0
while [ ! -d /partition ] && [ "$attempt" -lt 5 ]; do
    sleep 1
    attempt=$((attempt + 1))
done
[ -d /partition ] || fail

exercise_disk() {
    path=$1
    source="$path/source.bin"
    copy="$path/copy.bin"
    mkdir -p "$path" || return 1
    dd if=/dev/zero of="$source" bs=1048576 count=20 || return 1
    sync || return 1
    dd if="$source" of="$copy" bs=1048576 || return 1
    sync || return 1
    [ "$(wc -c < "$copy" | tr -d ' ')" = "20971520" ] || return 1
    [ "$(sha256sum "$source" | awk '{print $1}')" = "$(sha256sum "$copy" | awk '{print $1}')" ]
}

exercise_disk /root/ahci-port0 &
port0_pid=$!
exercise_disk /partition/ahci-port1 &
port1_pid=$!
wait "$port0_pid" || fail
wait "$port1_pid" || fail
echo 'AHCI_DUAL_NCQ_RW_20M_PASSED'
