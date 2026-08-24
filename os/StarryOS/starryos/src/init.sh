#!/bin/sh

export HOME=/root
export USER=root
export HOSTNAME=starry
export TERM=xterm-256color
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# AxVisor may attach a trusted, bounded cpio payload as /proc/initrd.  Extract
# it into tmpfs so board experiments can carry userspace binaries and data
# without modifying the persistent StarryOS root filesystem.
if [ -r /proc/initrd ]; then
    mkdir -p /tmp/axvisor-initrd
    cd /tmp/axvisor-initrd || exit 120
    initrd_magic="$(/bin/busybox od -An -N2 -tx1 /proc/initrd | /bin/busybox tr -d ' ')"
    if [ "$initrd_magic" = "1f8b" ]; then
        /bin/busybox gzip -dc /proc/initrd | /bin/busybox cpio -idmu
        extract_status=$?
    else
        /bin/busybox cpio -idmu < /proc/initrd
        extract_status=$?
    fi
    if [ "$extract_status" -eq 0 ]; then
        cd / || exit 121
        if [ -f /tmp/axvisor-initrd/init ]; then
            echo STARRY_AXVISOR_INITRD_PAYLOAD
            export AXVISOR_PAYLOAD_ROOT=/tmp/axvisor-initrd
            # Compatibility for the validated G2/G3 NPU payloads.
            export G2_PAYLOAD_ROOT="$AXVISOR_PAYLOAD_ROOT"
            exec /bin/sh "$AXVISOR_PAYLOAD_ROOT/init"
        fi
    else
        cd / || exit 122
        echo STARRY_AXVISOR_INITRD_EXTRACT_FAILED
    fi
fi

printf "Welcome to \033[96m\033[1mStarry OS\033[0m!\n"
env
echo

printf "Use \033[1m\033[3mapk\033[0m to install packages.\n"
echo

# Do your initialization here!

if [ -f /usr/bin/starry-run-case-tests ]; then
    echo "STARRY_GROUPED_AUTORUN_INIT"
    export AXBUILD_GROUPED_AUTORUN_DONE=1
    sh /usr/bin/starry-run-case-tests
fi

# Pre-populate /run/udev/data/ so libudev considers our devices
# "initialized" (otherwise libinput silently skips every input device
# with "skip unconfigured input device").  Linux populates this at udevd
# startup after rule processing; we don't run udevd.  One empty file per
# known device node — libudev flips is_initialized=true as soon as the
# file is openable, regardless of contents.
mkdir /run 2>/dev/null
mkdir /run/udev 2>/dev/null
mkdir /run/udev/data 2>/dev/null
# Use touch instead of : > redirect — POSIX shell exits on redirect failure
touch /run/udev/data/c226:0 2>/dev/null || true    # /dev/dri/card0
touch /run/udev/data/c29:0 2>/dev/null || true     # /dev/fb0 (if present)
for i in 0 1 2 3 4 5 6 7; do
    touch "/run/udev/data/c13:$((64 + i))" 2>/dev/null || true
done

# Visual-CI hook: when run_scenario.sh injects /test_runner.sh into the
# rootfs, fire it asynchronously before dropping to the login shell.
# Absence of /test_runner.sh in normal/interactive boots leaves this a
# true no-op, so this hook is harmless on user images.
#
# setsid detaches from the controlling tty so weston's children don't
# get SIGHUP when init re-execs the login shell; /dev/console captures
# the runner's progress prints into the serial log used by the harness
# to assert that the scenario actually launched.
if [ -x /test_runner.sh ]; then
    echo "[init] /test_runner.sh detected, launching visual scenario"
    setsid /test_runner.sh </dev/null >/dev/console 2>&1 &
    echo "[init] /test_runner.sh started pid=$!"
fi

cd "$HOME" || cd /

cat > /tmp/starry-shrc <<'EOF'
export PS1='${USER}@${HOSTNAME}:${PWD} # '
EOF
export ENV=/tmp/starry-shrc
exec /bin/sh -l -i
