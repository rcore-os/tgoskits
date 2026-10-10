#!/bin/bash
set -euo pipefail

marker="${STARRY_SELFBUILD_MARKER:-STARRY-ORANGEPI5PLUS-SELFBUILD}"
run_id="${STARRY_SELFBUILD_RUN_ID:-manual}"
profile="${STARRY_SELFBUILD_PROFILE:-off}"
profile_duration=300
source_dir="$(readlink -f /opt/tgoskits)"
build_config=apps/starry/orangepi-5-plus-selfbuild/build-aarch64-unknown-none-softfloat.toml
build_target=aarch64-unknown-none-softfloat
kernel_target_dir="$source_dir/target"
target_dir="/work/targets/${run_id}"
run_dir="/output/runs/${run_id}"
run_log="$run_dir/run.log"
xtask_bin="$target_dir/debug/tg-xtask"
progress_pid=""

fail() {
    printf '===%s-FAIL reason=%s===\n' "$marker" "$1"
    exit 1
}

compile_unit_count() {
    grep -c 'Compiling ' "$run_log" 2>/dev/null || true
}

distinct_crate_count() {
    sed -n 's/.*Compiling \([^ ]*\).*/\1/p' "$run_log" 2>/dev/null \
        | sort -u \
        | wc -l \
        | tr -d '[:space:]'
}

emit_progress() {
    local phase=$1
    local phase_start=$2
    local phase_base=$3
    local now total_units phase_units distinct_crates
    now="$(date +%s)"
    total_units="$(compile_unit_count)"
    phase_units="$((total_units - phase_base))"
    distinct_crates="$(distinct_crate_count)"
    printf '===%s-PROGRESS phase=%s elapsed=%s phase_compile_units=%s total_compile_units=%s distinct_crates=%s===\n' \
        "$marker" "$phase" "$((now - phase_start))" "$phase_units" "$total_units" \
        "$distinct_crates"
}

start_progress_sampler() {
    local phase=$1
    local phase_start=$2
    local phase_base=$3
    (
        while sleep 60; do
            emit_progress "$phase" "$phase_start" "$phase_base"
        done
    ) &
    progress_pid=$!
}

stop_progress_sampler() {
    if [ -n "$progress_pid" ] && kill -0 "$progress_pid" 2>/dev/null; then
        kill "$progress_pid" 2>/dev/null || true
        wait "$progress_pid" 2>/dev/null || true
    fi
    progress_pid=""
}

finish_profile() {
    local elapsed=$1
    local command_rc=$2
    local profile_artifact

    case "$profile" in
        stat) profile_artifact=perf-stat.txt ;;
        record) profile_artifact=perf.data ;;
        *) fail "cannot finish disabled profile" ;;
    esac
    [ -s "$run_dir/$profile_artifact" ] || fail "profile-artifact-missing-${profile_artifact}"

    cp .tgoskits-source-meta "$run_dir/source.meta"
    {
        printf 'profile=%s\n' "$profile"
        printf 'workload=cargo build -p tg-xtask\n'
        printf 'duration_limit_seconds=%s\n' "$profile_duration"
        printf 'elapsed_seconds=%s\n' "$elapsed"
        printf 'command_rc=%s\n' "$command_rc"
        printf 'parallelism=system-default\n'
    } > "$run_dir/profile.meta"
    (
        cd "$run_dir"
        sha256sum "$profile_artifact" profile.meta source.meta > SHA256SUMS
    )

    if [ "${KEEP_TARGET:-0}" != "1" ]; then
        if [ "$profile" = record ]; then
            # perf report resolves transient build-script DSOs from this tree.
            # Linux removes it only after the report has been generated.
            echo "===${marker}-PROFILE-TARGET-RETAINED path=${target_dir}==="
        else
            case "$target_dir" in
                /work/targets/*)
                    echo "===${marker}-TARGET-CLEANUP path=${target_dir}==="
                    rm -rf "$target_dir"
                    ;;
                *) fail unsafe-target-cleanup-path ;;
            esac
        fi
    fi
    printf '%s\n' "$run_id" > /output/latest-run
    sync

    echo "===${marker}-PROFILE-ARTIFACT path=$run_dir/$profile_artifact==="
    echo "===${marker}-PROFILE-PASS run=${run_id} profile=${profile} elapsed=${elapsed}==="
    echo "===${marker}-PASS run=${run_id} parallelism=system-default profile=${profile} elapsed=${elapsed}==="
    exit 0
}

case "$run_id" in
    ''|*[!A-Za-z0-9._-]*) fail invalid-run-id ;;
esac
case "$profile" in
    off|stat|record) ;;
    *) fail invalid-profile ;;
esac
[ -f "$source_dir/Cargo.toml" ] || fail source-missing
[ -f "$source_dir/.tgoskits-source-meta" ] || fail source-meta-missing
[ -f "$source_dir/$build_config" ] || fail build-config-missing
[ ! -e "$kernel_target_dir" ] || fail kernel-target-not-cold

bash "$source_dir/apps/starry/orangepi-5-plus-selfbuild/check_cold_run.sh" \
    "$run_dir" "$target_dir" "$marker"
exec > >(tee "$run_log") 2>&1
trap stop_progress_sampler EXIT

export HOME=/root
export CARGO_HOME=/root/.cargo
export PATH="$CARGO_HOME/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export CARGO_TARGET_DIR="$target_dir"
export CARGO_NET_OFFLINE=true

cd "$source_dir"

for command in \
    aarch64-linux-gnu-gcc aarch64-linux-musl-gcc cargo rustc rustup rust-nm rust-objcopy gen_ksym \
    file nproc python3 readelf sha256sum; do
    command -v "$command" >/dev/null 2>&1 || fail "tool-missing-${command}"
done
if [ "$profile" != off ]; then
    command -v perf >/dev/null 2>&1 || fail tool-missing-perf
    command -v timeout >/dev/null 2>&1 || fail tool-missing-timeout
fi

echo "===${marker}-BEGIN run=${run_id} parallelism=system-default profile=${profile}==="
echo "===${marker}-SOURCE-META-BEGIN==="
cat .tgoskits-source-meta
echo "===${marker}-SOURCE-META-END==="
rustc --version --verbose
cargo --version
rustup show active-toolchain
echo "logical_cpus=$(nproc --all)"
echo "available_cpus=$(nproc)"
echo "cpu_affinity=$(sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' /proc/self/status)"

probe_source=apps/starry/orangepi-5-plus-selfbuild/tests/aarch64_unaligned_access_probe.c
probe_binary="$target_dir/aarch64-unaligned-access-probe"
aarch64-linux-gnu-gcc -std=gnu11 -O2 -Wall -Wextra -Werror \
    "$probe_source" -o "$probe_binary" \
    || fail aarch64-unaligned-access-probe-build
"$probe_binary" || fail aarch64-unaligned-access-probe-run

overall_start="$(date +%s)"
xtask_command=(cargo build -p tg-xtask)
case "$profile" in
    off)
        xtask_run_command=("${xtask_command[@]}")
        ;;
    stat)
        xtask_run_command=(timeout --signal=INT --kill-after=30 "$profile_duration" perf stat -e cycles,instructions,cache-references,cache-misses,branches,branch-misses -o "$run_dir/perf-stat.txt" -- "${xtask_command[@]}")
        ;;
    record)
        xtask_run_command=(timeout --signal=INT --kill-after=30 "$profile_duration" perf record -F 49 -e cycles -o "$run_dir/perf.data" -- "${xtask_command[@]}")
        ;;
esac
printf '===%s-TG-XTASK-COMMAND===' "$marker"
printf ' %q' "${xtask_command[@]}"
printf '\n'
if [ "$profile" != off ]; then
    printf '===%s-PROFILE-COMMAND===' "$marker"
    printf ' %q' "${xtask_run_command[@]}"
    printf '\n'
fi
xtask_start="$(date +%s)"
xtask_compile_base="$(compile_unit_count)"
start_progress_sampler tg-xtask "$xtask_start" "$xtask_compile_base"
set +e
"${xtask_run_command[@]}"
xtask_rc="$?"
set -e
stop_progress_sampler
xtask_end="$(date +%s)"
xtask_elapsed="$((xtask_end - xtask_start))"
emit_progress tg-xtask "$xtask_start" "$xtask_compile_base"
echo "===${marker}-TG-XTASK-BUILD-END run=${run_id} rc=${xtask_rc} elapsed=${xtask_elapsed}==="
if [ "$profile" != off ]; then
    case "$xtask_rc" in
        0|124) finish_profile "$xtask_elapsed" "$xtask_rc" ;;
        *) fail "tg-xtask-profile rc=${xtask_rc}" ;;
    esac
fi
if [ "$xtask_rc" != "0" ]; then
    fail "tg-xtask-build rc=${xtask_rc}"
fi
[ -x "$xtask_bin" ] || fail tg-xtask-artifact-missing

export CARGO_TARGET_DIR="$kernel_target_dir"
build_command=("$xtask_bin" starry build --config "$build_config")
command=("${build_command[@]}")

printf '===%s-COMMAND===' "$marker"
printf ' %q' "${command[@]}"
printf '\n'
start="$(date +%s)"
starry_compile_base="$(compile_unit_count)"
start_progress_sampler starry "$start" "$starry_compile_base"
set +e
"${command[@]}"
rc="$?"
set -e
stop_progress_sampler
end="$(date +%s)"
elapsed="$((end - start))"
total_elapsed="$((end - overall_start))"
emit_progress starry "$start" "$starry_compile_base"
echo "===${marker}-STARRY-BUILD-END run=${run_id} rc=${rc} elapsed=${elapsed}==="
echo "===${marker}-BUILD-END run=${run_id} rc=${rc} elapsed=${total_elapsed}==="
if [ "$rc" != "0" ]; then
    fail "build rc=${rc}"
fi

artifact="$kernel_target_dir/$build_target/release/starryos"
artifact_bin="$artifact.bin"
[ -s "$artifact" ] || fail artifact-elf-missing
rust-objcopy --strip-all -O binary "$artifact" "$artifact_bin" \
    || fail artifact-bin-generation
[ -s "$artifact_bin" ] || fail artifact-bin-missing
file "$artifact" | grep -q 'ELF 64-bit.*ARM aarch64' || fail artifact-elf-architecture
readelf -h "$artifact" | grep -q 'Machine:.*AArch64' || fail artifact-readelf-architecture

cp "$artifact" "$run_dir/starryos.elf"
cp "$artifact_bin" "$run_dir/starryos.bin"
cp .tgoskits-source-meta "$run_dir/source.meta"
printf '%s\n' "$xtask_elapsed" > "$run_dir/tg-xtask-elapsed-seconds"
printf '%s\n' "$elapsed" > "$run_dir/starry-build-elapsed-seconds"
printf '%s\n' "$total_elapsed" > "$run_dir/elapsed-seconds"
(
    cd "$run_dir"
    sha256sum starryos.elf starryos.bin source.meta > SHA256SUMS
)
if [ "${KEEP_TARGET:-0}" != "1" ]; then
    case "$kernel_target_dir" in
        /opt/tgoskits-sources/*/target)
            echo "===${marker}-TARGET-CLEANUP path=${kernel_target_dir}==="
            rm -rf "$kernel_target_dir"
            ;;
        *) fail unsafe-kernel-target-cleanup-path ;;
    esac
    case "$target_dir" in
        /work/targets/*)
            echo "===${marker}-TARGET-CLEANUP path=${target_dir}==="
            rm -rf "$target_dir"
            ;;
        *) fail unsafe-target-cleanup-path ;;
    esac
fi
printf '%s\n' "$run_id" > /output/latest-run
sync

echo "===${marker}-ARTIFACT elf=$run_dir/starryos.elf bin=$run_dir/starryos.bin==="
echo "===${marker}-PASS run=${run_id} parallelism=system-default elapsed=${total_elapsed}==="
