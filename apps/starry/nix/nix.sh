#!/bin/sh
set -eu

# Create Nix subcommand symlinks (overlay doesn't support symlinks).
for cmd in build channel collect-garbage copy-closure env hash \
           instantiate prefetch-url shell store; do
    ln -sf nix /usr/bin/nix-$cmd 2>/dev/null || true
done

DIAG_PID=$$
echo "NIX_DIAG_SCRIPT_PID=$DIAG_PID"

fail() {
    echo "NIX_ERROR: $1"
    echo 'NIX_TEST_FAILED'
    exit 1
}

# Dump process state for a given PID.
dump_proc_state() {
    pid="$1"
    label="$2"
    echo "NIX_DIAG_PROCSTAT ${label}_PID=${pid}"
    echo "NIX_DIAG_PROCSTAT ${label}_STATUS_BEGIN"
    cat "/proc/$pid/status" 2>/dev/null || echo "NIX_DIAG_NO_PROC_STATUS"
    echo "NIX_DIAG_PROCSTAT ${label}_STATUS_END"
    echo "NIX_DIAG_PROCSTAT ${label}_STAT_BEGIN"
    cat "/proc/$pid/stat" 2>/dev/null || echo "NIX_DIAG_NO_PROC_STAT"
    echo "NIX_DIAG_PROCSTAT ${label}_STAT_END"
}

# Dump per-thread state. Nix keeps a small worker thread pool, so the stuck
# waiter can be a different tid.
dump_thread_states() {
    pid="$1"
    label="$2"
    echo "NIX_DIAG_THREADS ${label}_BEGIN"
    for tdir in "/proc/$pid/task"/*/; do
        tid=$(basename "$tdir")
        case "$tid" in *[!0-9]*) continue ;; esac
        comm=$(cat "$tdir/comm" 2>/dev/null || echo '?')
        stat=$(cat "$tdir/stat" 2>/dev/null || echo '?')
        echo "TID=$tid COMM=$comm STAT=$stat"
        echo "NIX_DIAG_THREAD_FD ${label}_${tid}_BEGIN"
        ls -la "$tdir/fd" 2>/dev/null || echo 'NIX_DIAG_NO_THREAD_FD'
        echo "NIX_DIAG_THREAD_FD ${label}_${tid}_END"
    done
    echo "NIX_DIAG_THREADS ${label}_END"
}

# Lightweight poll: just the stat line fields that show CPU progress
poll_proc_progress() {
    pid="$1"
    label="$2"
    stat_line=$(cat "/proc/$pid/stat" 2>/dev/null || echo '?')
    # Fields: pid comm state ppid ... utime(14) stime(15) cutime(16) cstime(17) ...
    echo "NIX_DIAG_PROGRESS ${label} ${stat_line}"
}

# Dump all processes state overview
dump_all_procs() {
    echo 'NIX_DIAG_ALLPROC_BEGIN'
    for pdir in /proc/*/; do
        pid=$(basename "$pdir")
        case "$pid" in *[!0-9]*) continue ;; esac
        name=$(cat "$pdir/comm" 2>/dev/null || echo '?')
        state=$(awk '{print $3}' "$pdir/stat" 2>/dev/null || echo '?')
        echo "PID=$pid NAME=$name STATE=$state"
    done
    echo 'NIX_DIAG_ALLPROC_END'
}

# Check for zombie processes
dump_zombies() {
    echo 'NIX_DIAG_ZOMBIE_BEGIN'
    ps -eo pid,ppid,state,comm 2>/dev/null | grep -E 'Z|zombie|defunct' || echo 'NIX_DIAG_NO_ZOMBIES'
    echo 'NIX_DIAG_ZOMBIE_END'
}

# Dump Nix store database status
dump_nix_db() {
    echo 'NIX_DIAG_NIX_DB_BEGIN'
    echo "NIX_DB_SIZE=$(wc -c < /nix/var/nix/db/db.sqlite 2>/dev/null || echo '?')"
    echo "NIX_DB_WAL_SIZE=$(wc -c < /nix/var/nix/db/db.sqlite-wal 2>/dev/null || echo '?')"
    ls -la /nix/var/nix/db/ 2>/dev/null || echo 'NIX_DIAG_NO_DB_DIR'
    echo 'NIX_DIAG_NIX_DB_END'
}

echo 'NIX_PHASE_ROOTFS_BEGIN'
mkdir -p /nix /etc/nix /tmp/nix || fail 'failed to create Nix smoke directories'

echo 'NIX_PHASE_PREBUILT_NIX_BEGIN'
command -v nix >/dev/null 2>&1 || fail 'prebuilt Nix is missing from case rootfs'
echo 'NIX_PHASE_PREBUILT_NIX_DONE'

echo 'NIX_PHASE_NIX_BEGIN'
nix --version || fail 'nix --version failed'
echo 'NIX_PHASE_NIX_DONE'

echo 'NIX_PHASE_BUILD_BEGIN'
rm -f ./result
cat > /tmp/nix/default.nix <<'EOF'
derivation {
  name = "nix";
  system = builtins.currentSystem;
  builder = "/bin/sh";
  args = [ "-c" "mkdir -p /tmp/nix; echo BUILDER_STARTED > /tmp/nix/builder.log; echo OUT=$out >> /tmp/nix/builder.log; echo NIX_LOCAL_BUILD_OK > $out" ];
}
EOF
echo 'NIX_INFO: tiny local sandboxed nix-build timeout is 120s'
# Run with -vvvvv for verbose Nix internal logging
nix-build -vvvvv --no-substitute --option build-users-group '' --option sandbox true /tmp/nix/default.nix >/tmp/nix/build.log 2>&1 &
build_pid=$!
build_rc=0
build_diag_done=0
prev_utime=0
prev_stime=0
for i in $(seq 1 120); do
    if ! kill -0 "$build_pid" 2>/dev/null; then
        set +e
        wait "$build_pid"
        build_rc=$?
        set -e
        break
    fi
    # Lightweight CPU progress poll every 10s starting at t=5
    if [ "$((i % 10))" -eq 5 ] 2>/dev/null; then
        poll_proc_progress "$build_pid" "T${i}"
    fi
    if [ "$i" -eq 30 ] && [ "$build_diag_done" -eq 0 ]; then
        echo 'NIX_DIAG_RUNNING_BEGIN'
        ps
        echo 'NIX_DIAG_RUNNING_FIND_BEGIN'
        find /nix/store -maxdepth 1 -name '*-nix' -exec ls -li {} \;
        find /nix/store -maxdepth 1 -name '*-nix' -exec stat {} \; 2>/dev/null || true
        echo 'NIX_DIAG_RUNNING_FIND_END'
        echo 'NIX_DIAG_RUNNING_FD_BEGIN'
        ls -la "/proc/$build_pid/fd" 2>/dev/null || echo 'NIX_DIAG_NO_PROC_FD'
        echo 'NIX_DIAG_RUNNING_FD_END'
        echo 'NIX_DIAG_RUNNING_BUILD_LOG_TAIL_BEGIN'
        tail -40 /tmp/nix/build.log
        echo 'NIX_DIAG_RUNNING_BUILD_LOG_TAIL_END'
        echo 'NIX_DIAG_PROCESS_BEGIN'
        dump_proc_state "$build_pid" 'NIX_BUILD_T30'
        dump_thread_states "$build_pid" 'NIX_BUILD_T30'
        dump_all_procs
        dump_zombies
        dump_nix_db
        echo 'NIX_DIAG_PROCESS_END'
        echo 'NIX_DIAG_RESULT_SYMLINK_BEGIN'
        ls -la ./result 2>/dev/null || echo 'NIX_DIAG_NO_RESULT_SYMLINK'
        echo 'NIX_DIAG_RESULT_SYMLINK_END'
        echo 'NIX_DIAG_RUNNING_END'
        build_diag_done=1
    fi
    # Second diag at t=80: full process state + verbose build log tail
    if [ "$i" -eq 80 ] && [ "$build_diag_done" -eq 1 ]; then
        if kill -0 "$build_pid" 2>/dev/null; then
            echo 'NIX_DIAG_T80_BEGIN'
            dump_proc_state "$build_pid" 'NIX_BUILD_T80'
            dump_thread_states "$build_pid" 'NIX_BUILD_T80'
            dump_all_procs
            dump_zombies
            dump_nix_db
            echo 'NIX_DIAG_T80_BUILD_LOG_TAIL_BEGIN'
            tail -60 /tmp/nix/build.log
            echo 'NIX_DIAG_T80_BUILD_LOG_TAIL_END'
            echo 'NIX_DIAG_T80_RESULT_SYMLINK_BEGIN'
            ls -la ./result 2>/dev/null || echo 'NIX_DIAG_NO_RESULT_SYMLINK'
            echo 'NIX_DIAG_T80_RESULT_SYMLINK_END'
            echo 'NIX_DIAG_T80_END'
        fi
    fi
    sleep 1
done
if kill -0 "$build_pid" 2>/dev/null; then
    kill "$build_pid" 2>/dev/null || true
    wait "$build_pid" 2>/dev/null || true
    build_rc=124
fi
if [ "$build_rc" -ne 0 ]; then
    echo 'NIX_DIAG_BUILDER_LOG_BEGIN'
    cat /tmp/nix/builder.log 2>/dev/null || echo 'NIX_DIAG_NO_BUILDER_LOG'
    echo 'NIX_DIAG_BUILDER_LOG_END'
    echo 'NIX_DIAG_BUILD_LOG_FINAL_BEGIN'
    cat /tmp/nix/build.log
    echo 'NIX_DIAG_BUILD_LOG_FINAL_END'
    echo 'NIX_DIAG_PS_BEGIN'
    ps
    echo 'NIX_DIAG_PS_END'
    echo 'NIX_DIAG_PROCESS_FINAL_BEGIN'
    dump_all_procs
    dump_zombies
    dump_nix_db
    echo 'NIX_DIAG_PROCESS_FINAL_END'
    echo 'NIX_DIAG_FIND_BEGIN'
    find /nix/store -maxdepth 1 -name '*-nix' -exec ls -la {} \;
    echo 'NIX_DIAG_FIND_END'
    echo 'NIX_DIAG_INODE_BEGIN'
    find /nix/store -maxdepth 1 -name '*-nix' -exec ls -li {} \;
    find /nix/store -maxdepth 1 -name '*-nix' -exec stat {} \; 2>/dev/null || true
    echo 'NIX_DIAG_INODE_END'
    echo "NIX_BUILD_EXIT=$build_rc"
    if [ "$build_rc" -eq 124 ] || [ "$build_rc" -eq 143 ]; then
        fail 'tiny local sandboxed nix-build timed out after 120s'
    fi
    if grep -q 'interrupted by the user' /tmp/nix/build.log; then
        fail 'tiny local sandboxed nix-build was interrupted after waiting for store lock'
    fi
    fail 'tiny local sandboxed nix-build failed'
fi
cat /tmp/nix/build.log
# Detect sandbox auto-disabled by Nix (missing mount namespace isolation).
# When sandbox is silently disabled, the build may still succeed, but the
# claimed sandboxed-build behavior was never exercised.  This test MUST FAIL
# in that case so reviewers can see whether namespace support is ready.
if grep -qi 'disabling sandbox\|sandbox.*disabled\|sandbox.*not supported' /tmp/nix/build.log; then
    echo 'NIX_INFO: sandbox was disabled by Nix (namespace isolation missing)'
    fail 'nix-build sandbox was disabled unexpectedly — mount namespace isolation missing'
fi
cat ./result || fail 'nix-build result symlink could not be read'
grep -q 'NIX_LOCAL_BUILD_OK' ./result || fail 'tiny local nix-build output marker missing'
echo 'NIX_PHASE_BUILD_DONE'
echo 'NIX_TEST_PASSED'
