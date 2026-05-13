#!/bin/sh

export HOME=/root
export USER=root
export HOSTNAME=starry
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

printf "Welcome to \033[96m\033[1mStarry OS\033[0m!\n"
env
echo

printf "Use \033[1m\033[3mapk\033[0m to install packages.\n"
echo

# Do your initialization here!

# Pre-populate /run/udev/data/ so libudev considers our devices
# "initialized" (otherwise libinput silently skips every input device
# with "skip unconfigured input device").  Linux populates this at udevd
# startup after rule processing; we don't run udevd.  One empty file per
# known device node — libudev flips is_initialized=true as soon as the
# file is openable, regardless of contents.
mkdir /run 2>/dev/null
mkdir /run/udev 2>/dev/null
mkdir /run/udev/data 2>/dev/null
: > /run/udev/data/c226:0     # /dev/dri/card0
: > /run/udev/data/c29:0      # /dev/fb0 (if present)
for i in 0 1 2 3 4 5 6 7; do
    : > "/run/udev/data/c13:$((64 + i))" 2>/dev/null
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
