#!/bin/sh
set -eu

fail() {
    echo "STARRY_LINUX_PERF_FAILED: $*"
    exit 1
}

workload="${LINUX_PERF_WORKLOAD:?LINUX_PERF_WORKLOAD is required}"
work_dir="/tmp/starry-linux-perf-e2e-$$"
runtime="$work_dir/runtime"
loader="$runtime/lib/ld-musl-aarch64.so.1"
perf_binary="$runtime/usr/bin/perf"

cleanup() {
    rm -rf "$work_dir"
}
trap cleanup EXIT INT TERM

mkdir -p "$runtime"
archive="${LINUX_PERF_RUNTIME_ARCHIVE:-}"
if [ -z "$archive" ]; then
    archive="$work_dir/runtime.tar.gz"
    archive_parts="${LINUX_PERF_RUNTIME_ARCHIVE_PARTS:?LINUX_PERF_RUNTIME_ARCHIVE_PARTS is required}"
    : > "$archive"
    for part in $archive_parts; do
        [ -s "$part" ] || fail "missing runtime archive part $part"
        cat "$part" >> "$archive" || fail "join runtime archive"
    done
fi
# StarryOS intentionally does not yet implement every tar metadata operation.
# BusyBox tar may therefore report a non-fatal directory chmod error after it
# has extracted the full tree.  Skip owner/time restoration and validate the
# executable payload below instead of treating that metadata-only status as a
# perf failure.
if ! tar -xmzof "$archive" -C "$runtime" 2> "$work_dir/tar.log"; then
    echo "linux-perf: tar reported unsupported metadata operations"
    sed -n '1,8p' "$work_dir/tar.log"
fi
[ -x "$loader" ] || fail "missing musl loader"
[ -x "$perf_binary" ] || fail "missing perf binary"
[ -x "$workload" ] || fail "missing workload"

runtime_library_path="$runtime/lib:$runtime/usr/lib"
export LD_LIBRARY_PATH="$runtime_library_path"
export PERF_EXEC_PATH="$runtime/usr/libexec/perf-core"
export TRACEEVENT_PLUGIN_DIR="$runtime/usr/lib/traceevent/plugins"

run_perf() {
    "$loader" --library-path "$runtime_library_path" "$perf_binary" "$@"
}

run_perf --version > "$work_dir/version.log" 2>&1 || fail "perf --version"
grep -q "perf version 6.19.14" "$work_dir/version.log" || fail "unexpected perf version"

if ! run_perf stat -e cycles -e task-clock -- "$workload" \
    > "$work_dir/stat.out" 2> "$work_dir/stat.log"; then
    sed -n '1,120p' "$work_dir/stat.log"
    fail "perf stat"
fi
grep -Eq '[1-9][0-9,]*[[:space:]]+([^[:space:]]+/)?cycles/?' "$work_dir/stat.log" || {
    sed -n '1,120p' "$work_dir/stat.log"
    fail "cycles did not count"
}
grep -Eq '([1-9][0-9,]*(\.[0-9]+)?|0\.[0-9]*[1-9][0-9]*)[[:space:]]+(msec[[:space:]]+)?task-clock' "$work_dir/stat.log" || {
    sed -n '1,120p' "$work_dir/stat.log"
    fail "task-clock did not count"
}

data="$work_dir/perf.data"
if ! run_perf record -a -g --call-graph fp -e cycles:u -c 1000000 -o "$data" -- "$workload" \
    > "$work_dir/record.out" 2> "$work_dir/record.log"; then
    sed -n '1,120p' "$work_dir/record.log"
    fail "perf record -a"
fi
[ -s "$data" ] || fail "empty perf.data"

if ! run_perf report --stdio --no-children --sort comm,dso,symbol -i "$data" \
    > "$work_dir/report.log" 2>&1; then
    sed -n '1,160p' "$work_dir/report.log"
    fail "perf report --stdio"
fi
grep -q 'perf_leaf' "$work_dir/report.log" || {
    cat "$work_dir/record.out"
    sed -n '1,120p' "$work_dir/record.log"
    sed -n '1,160p' "$work_dir/report.log"
    fail "missing sampled workload symbols"
}
grep -Eq '^# Samples:[[:space:]]*[1-9][0-9]*' "$work_dir/report.log" || {
    sed -n '1,160p' "$work_dir/report.log"
    fail "perf report contains no samples"
}
grep -Eq '^[[:space:]]*([1-9][0-9]*(\.[0-9]+)?|0\.[0-9]*[1-9][0-9]*)%.*perf_leaf' \
    "$work_dir/report.log" || {
    sed -n '1,160p' "$work_dir/report.log"
    fail "sampled workload has zero overhead"
}
for frame in perf_level_one perf_level_two perf_level_three; do
    grep -q "$frame" "$work_dir/report.log" || {
        sed -n '1,200p' "$work_dir/report.log"
        fail "missing user callchain frame $frame"
    }
done

if [ "${LINUX_PERF_BOARD:-0}" = "1" ]; then
    cpu_count="$(grep -c '^processor' /proc/cpuinfo || true)"
    [ "$cpu_count" = "8" ] || fail "expected 8 OrangePi CPUs, got $cpu_count"
    grep -Eqi 'CPU part[[:space:]]*:[[:space:]]*0xd05' /proc/cpuinfo \
        || fail "Cortex-A55 MIDR is not visible"
    grep -Eqi 'CPU part[[:space:]]*:[[:space:]]*0xd0b' /proc/cpuinfo \
        || fail "Cortex-A76 MIDR is not visible"
    [ -r /sys/bus/event_source/devices/armv8_cortex_a55/cpus ] \
        || fail "missing Cortex-A55 PMU source"
    [ -r /sys/bus/event_source/devices/armv8_cortex_a76/cpus ] \
        || fail "missing Cortex-A76 PMU source"
    grep -q '0' /sys/bus/event_source/devices/armv8_cortex_a55/cpus \
        || fail "Cortex-A55 PMU cpumask"
    grep -Eq '4|6' /sys/bus/event_source/devices/armv8_cortex_a76/cpus \
        || fail "Cortex-A76 PMU cpumask"

    if ! run_perf stat -e cycles -e cpu-migrations -- "$workload" --migrate \
        > "$work_dir/migrate.out" 2> "$work_dir/migrate.log"; then
        sed -n '1,160p' "$work_dir/migrate.log"
        fail "A55 to A76 migration"
    fi
    grep -q 'STARRY_LINUX_PERF_WORKLOAD_MIGRATED' "$work_dir/migrate.out" \
        || fail "migration workload did not finish"
    grep -Eq '[1-9][0-9,]*[[:space:]]+cpu-migrations' "$work_dir/migrate.log" \
        || fail "CPU migration counter did not count"

    if ! taskset -c 0 "$loader" --library-path "$runtime_library_path" \
        "$perf_binary" stat \
        -e armv8_cortex_a55/cpu_cycles/ \
        -e armv8_cortex_a55/cpu_cycles/ \
        -e armv8_cortex_a55/cpu_cycles/ \
        -e armv8_cortex_a55/cpu_cycles/ \
        -e armv8_cortex_a55/cpu_cycles/ \
        -e armv8_cortex_a55/cpu_cycles/ \
        -e armv8_cortex_a55/cpu_cycles/ \
        -e armv8_cortex_a55/cpu_cycles/ \
        -- "$workload" > "$work_dir/mux.out" 2> "$work_dir/mux.log"; then
        sed -n '1,180p' "$work_dir/mux.log"
        fail "Cortex-A55 multiplex"
    fi
    mux_values="$(grep -Ec '[1-9][0-9,]*[[:space:]]+armv8_cortex_a55/cpu_cycles/' \
        "$work_dir/mux.log" || true)"
    [ "$mux_values" = "8" ] || fail "expected 8 multiplexed counter values, got $mux_values"
    grep 'armv8_cortex_a55/cpu_cycles/' "$work_dir/mux.log" \
        | grep -Eq '\([0-9]{1,2}\.[0-9]+%\)' \
        || fail "Cortex-A55 events were not multiplexed"

    for cpu in 0 4; do
        if ! taskset -c "$cpu" "$loader" --library-path "$runtime_library_path" \
            "$perf_binary" stat \
            -e cycles -e instructions -e cache-misses -e branch-instructions \
            -- "$workload" > "$work_dir/board-$cpu.out" 2> "$work_dir/board-$cpu.log"; then
            sed -n '1,120p' "$work_dir/board-$cpu.log"
            fail "CPU $cpu hardware events"
        fi
        grep -Eq '[1-9][0-9,]*[[:space:]]+([^[:space:]]+/)?cycles/?' "$work_dir/board-$cpu.log" \
            || fail "CPU $cpu cycles"
        grep -Eq '[1-9][0-9,]*[[:space:]]+([^[:space:]]+/)?instructions/?' "$work_dir/board-$cpu.log" \
            || fail "CPU $cpu instructions"
        grep -Eq '[1-9][0-9,]*[[:space:]]+([^[:space:]]+/)?cache-misses/?' "$work_dir/board-$cpu.log" \
            || fail "CPU $cpu cache misses"
        grep -Eq '[1-9][0-9,]*[[:space:]]+([^[:space:]]+/)?branch-instructions/?' "$work_dir/board-$cpu.log" \
            || fail "CPU $cpu branch instructions"
    done
fi

echo STARRY_LINUX_PERF_PASSED
