#!/bin/sh
set -eu

log_dir="/tmp/test-gdb-native-batch"
mkdir -p "$log_dir"

standalone_log="$log_dir/standalone.log"
gdb_log="$log_dir/gdb.log"
ptrace_log="$log_dir/ptrace.log"

printf 'clear\n' >/proc/ptrace_debug_log 2>/dev/null || true

standalone_status=0
if ! /usr/bin/test-gdb-native-batch-target >"$standalone_log" 2>&1; then
    standalone_status=$?
fi

echo "=== STANDALONE LOG BEGIN ==="
cat "$standalone_log"
echo "=== STANDALONE LOG END (exit=$standalone_status) ==="

gdb_status=0
if ! /usr/bin/gdb -q -batch -x /usr/bin/gdb-native-batch.gdb /usr/bin/test-gdb-native-batch-target >"$gdb_log" 2>&1; then
    gdb_status=$?
fi

echo "=== GDB LOG BEGIN ==="
cat "$gdb_log"
echo "=== GDB LOG END (exit=$gdb_status) ==="
cat /proc/ptrace_debug_log >"$ptrace_log" 2>&1 || true
echo "=== PTRACE LOG BEGIN ==="
cat "$ptrace_log"
echo "=== PTRACE LOG END ==="
echo "Logs saved under $log_dir"

if [ "$standalone_status" -ne 0 ]; then
    echo "FAIL: standalone target exited with $standalone_status"
    exit 1
fi

if [ "$gdb_status" -ne 0 ]; then
    echo "FAIL: gdb exited with $gdb_status"
    exit 1
fi

if grep -Eq '^Program (received|terminated with) signal ' "$gdb_log"; then
    echo "FAIL: gdb inferior terminated by signal"
    exit 1
fi

printf 'GDB_NATIVE_BATCH_DONE\n'
