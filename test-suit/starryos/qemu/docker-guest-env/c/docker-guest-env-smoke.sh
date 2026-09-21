#!/bin/sh
# Phase 1 guest smoke of the StarryOS docker bring-up plan
# (docs/design/docker-startup.md): exercises the Debian userland tools that
# the container stack depends on. Exact kernel semantics live in the
# docker-guest-env-probe binary; every shell-level failure is marked with
# DGE_SHELL_FAIL and counted into the exit status.

fails=0
mark() {
    echo "DGE_SHELL_FAIL $1"
    fails=$((fails + 1))
}

echo DOCKER_GUEST_ENV_SHELL_BEGIN

# Debian userland sanity: bash, coreutils, util-linux.
/bin/bash --norc -c 'echo bash-ok:$BASH_VERSION' || mark bash
for tool in mount unshare nsenter; do
    command -v "$tool" >/dev/null 2>&1 || mark "util-$tool"
done

# The kernel-semantics probe runs first so its per-check errnos land in the
# serial log before any fail marker short-captures the run.
/usr/bin/docker-guest-env-probe || mark probe

# /proc/filesystems must advertise what the kernel mounts; util-linux mount
# and container runtimes consult it.
grep -q cgroup2 /proc/filesystems || mark proc-filesystems-cgroup2
grep -q devpts /proc/filesystems || mark proc-filesystems-devpts

# Pseudofs mountpoints the kernel mounts at boot; superblock types are probed
# in C via statfs.
grep -q ' /proc ' /proc/mounts || mark proc
grep -q ' /sys ' /proc/mounts || mark sysfs
grep -q ' /dev ' /proc/mounts || mark devfs
grep -q ' /dev/shm ' /proc/mounts || mark devshm
grep -q ' /tmp ' /proc/mounts || mark tmp

# The kernel devfs ships the ptmx node and the dynamic pts directory; do not
# mount devpts over /dev/pts, that would shadow the devfs directory and split
# ptmx from its slaves. Container-style newinstance devpts mounts are probed
# in C under /tmp.
test -c /dev/ptmx || mark ptmx-node
test -d /dev/pts || mark pts-dir

# Userland mounts: a fresh tmpfs (kernel /tmp is already tmpfs, so mount a
# new directory to make the check meaningful) and cgroup2 at the /sys/fs/cgroup
# mountpoint pre-created by sysfs.
mkdir -p /mnt/dge-tmpfs || mark tmpfs-mntdir
mount -t tmpfs tmpfs /mnt/dge-tmpfs || mark tmpfs-mount
echo dge-tmpfs > /mnt/dge-tmpfs/marker 2>/dev/null &&
    grep -q dge-tmpfs /mnt/dge-tmpfs/marker ||
    mark tmpfs-rw
mkdir -p /sys/fs/cgroup || mark cgroup2-mntdir
mount -t cgroup2 none /sys/fs/cgroup || mark cgroup2-mount
# cgroup.controllers is a dynamic procfs-style file: st_size is always 0, so
# test the content, not the size.
grep -q pids /sys/fs/cgroup/cgroup.controllers || mark cgroup2-controllers

# util-linux namespace smoke; exact unshare/setns semantics are probed in C.
unshare -p -f sh -c 'test "$$" = 1' || mark unshare-pid
unshare -n true || mark unshare-net
nsenter -t 1 -p true || mark nsenter-pid
nsenter -t 1 -n true || mark nsenter-net

# The pass/fail marker lives here, not in the interactive shell_init_cmd: a
# multi-line if/fi typed at the prompt gets PS2 continuation prompts glued to
# the marker line, which breaks the case's line-anchored success_regex.
if [ "$fails" -eq 0 ]; then
    echo DOCKER_GUEST_ENV_PASSED
else
    echo "DOCKER_GUEST_ENV_FAILED: status=$fails"
    exit 1
fi
