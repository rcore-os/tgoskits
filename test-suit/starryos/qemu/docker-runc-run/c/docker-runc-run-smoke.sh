#!/bin/sh
# Phase 2 driver of the StarryOS docker bring-up plan
# (docs/design/docker-startup.md): runs a real runc against busybox OCI
# bundles. Kernel semantics are gated by the docker-runc-run-probe binary;
# every shell-level failure is marked with DOCKER_RUNC_RUN_SHELL_FAIL and
# counted into the final marker. All markers print from this file (never from
# a multi-line shell_init_cmd block — PS2 prompts glue to the marker line).

fails=0
mark() {
    echo "DOCKER_RUNC_RUN_SHELL_FAIL $1"
    fails=$((fails + 1))
}

echo DOCKER_RUNC_RUN_SHELL_BEGIN

# runc presence and version gate.
test -x /usr/sbin/runc || mark runc-binary
/usr/sbin/runc --version 2>&1 | grep -q "runc version" || mark runc-version

# Kernel-semantics probe (starttime, oom NUL write, memfd mode, pipe fchown,
# stage-B gate).
/usr/bin/docker-runc-run-probe || mark probe

mkdir -p /run/runc /tmp/drr || mark workdir
# runc needs a cgroup2 mount visible in /proc/self/mountinfo to pick the v2
# manager; the mountpoint is pre-created by sysfs (see Phase 1).
mount -t cgroup2 none /sys/fs/cgroup || mark cgroup2-mount

# Runs one runc bundle; dumps the debug log to the serial before any fail
# marker so a failing run leaves its evidence trail. --root/--debug are
# global runc flags and must precede the `run` subcommand.
run_runc() {
    bundle="$1"
    root="$2"
    id="$3"
    log="$4"
    extra="$5"
    cd "$bundle" || return 99
    # shellcheck disable=SC2086
    timeout -k 5 60 runc --debug --root "$root" $extra run --no-new-keyring "$id" < /dev/null > "$log" 2>&1
    rc=$?
    cd /
    if [ "$rc" -ne 0 ]; then
        echo "---- runc log ($id) rc=$rc ----"
        cat "$log"
        echo "---- end runc log ----"
    fi
    return "$rc"
}

# ---- Stage A: cgroups disabled ----
run_runc /opt/hello-bundle /tmp/drr/a helloA /tmp/drr/a.log --rootless=true
rc=$?
if [ "$rc" -ne 0 ] || ! grep -q hello-from-container /tmp/drr/a.log; then
    mark "runc-echo-rc=$rc"
fi

run_runc /opt/exit1-bundle /tmp/drr/b exitB /tmp/drr/b.log --rootless=true
rc=$?
if [ "$rc" -ne 3 ]; then
    echo "---- runc log (exitB) rc=$rc ----"
    cat /tmp/drr/b.log
    echo "---- end runc log ----"
    mark "runc-exit-code=$rc"
fi

run_runc /opt/ns-bundle /tmp/drr/c nsC /tmp/drr/c.log --rootless=true
rc=$?
if [ "$rc" -ne 0 ] || ! grep -q DOCKER_RUNC_RUN_NS_OK /tmp/drr/c.log; then
    mark "runc-ns-rc=$rc"
fi

# ---- Stage B: cgroups enabled (bpf device-controller stub required) ----
stageb=$(cat /tmp/drr-stageb 2>/dev/null || echo 0)
if [ "$stageb" = "1" ]; then
    echo DOCKER_RUNC_RUN_STAGE_B_ENABLED
    cd /opt/pids-bundle || {
        mark pids-bundle-dir
        exit 1
    }
    runc --root /tmp/drr/d run --no-new-keyring pidsC < /dev/null > /tmp/drr/d.log 2>&1
    rc=$?
    if grep -q DOCKER_RUNC_RUN_PIDS_EAGAIN_OK /tmp/drr/d.log && [ "$rc" -eq 0 ]; then
        echo DOCKER_RUNC_RUN_STAGE_B_OK
    else
        echo "DOCKER_RUNC_RUN_STAGE_B_FAILED: rc=$rc"
        echo "---- runc log (pidsC) rc=$rc ----"
        cat /tmp/drr/d.log
        echo "---- end runc log ----"
        fails=$((fails + 1))
    fi
else
    echo DOCKER_RUNC_RUN_STAGE_B_SKIPPED
fi

cd /
rm -rf /tmp/drr

# The pass/fail marker lives here, not in the interactive shell_init_cmd: a
# multi-line if/fi typed at the prompt gets PS2 continuation prompts glued to
# the marker line, which breaks the case's line-anchored success_regex.
if [ "$fails" -eq 0 ]; then
    echo DOCKER_RUNC_RUN_PASSED
else
    echo "DOCKER_RUNC_RUN_FAILED: status=$fails"
    exit 1
fi
