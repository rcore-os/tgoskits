#!/bin/bash
set -euo pipefail

export LC_ALL=C
export LANG=C

marker="${STARRY_SELFBUILD_MARKER:-STARRY-ORANGEPI5PLUS-SELFBUILD}"
run_id="${STARRY_SELFBUILD_RUN_ID:-manual}"
source_dir="$(readlink -f /opt/tgoskits)"
build_config=apps/starry/orangepi-5-plus-selfbuild/build-aarch64-unknown-none-softfloat.toml
build_target=aarch64-unknown-none-softfloat
kernel_target_dir="/work/targets/${run_id}-starry"
target_dir="/work/targets/${run_id}"
run_dir="/output/runs/${run_id}"
run_log="$run_dir/run.log"
xtask_bin="$target_dir/debug/tg-xtask"

fail() {
    printf '===%s-FAIL reason=%s===\n' "$marker" "$1"
    exit 1
}


case "$run_id" in
    ''|*[!A-Za-z0-9._-]*) fail invalid-run-id ;;
esac
[ -f "$source_dir/Cargo.toml" ] || fail source-missing
[ -f "$source_dir/.tgoskits-source-meta" ] || fail source-meta-missing
[ -f "$source_dir/$build_config" ] || fail build-config-missing
[ ! -e "$kernel_target_dir" ] || fail kernel-target-not-cold

bash "$source_dir/apps/starry/orangepi-5-plus-selfbuild/check_cold_run.sh" \
    "$run_dir" "$target_dir" "$marker"
source "$source_dir/apps/starry/orangepi-5-plus-selfbuild/selfbuild_log.sh"
begin_build_log "$run_log" "$marker"

export HOME=/root
export CARGO_HOME=/work/cargo-home
export RUSTUP_HOME=/root/.rustup
export TMPDIR=/work/tmp
mkdir "$CARGO_HOME" "$TMPDIR"
# Keep the downloaded SDK immutable; Cargo owns only locks and usage metadata.
ln -s /root/.cargo/bin "$CARGO_HOME/bin"
ln -s /root/.cargo/registry "$CARGO_HOME/registry"
cp /root/.cargo/config.toml "$CARGO_HOME/config.toml"
export PATH="$CARGO_HOME/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export CARGO_TARGET_DIR="$target_dir"
export CARGO_NET_OFFLINE=true
# Both targets are discarded after this cold run, so incremental state only
# adds compilation work and consumes the native target's temporary storage.
export CARGO_INCREMENTAL=0

cd "$source_dir"
export CARGO="$(command -v cargo)"
sysroot=$(rustc --print sysroot)
export LD_LIBRARY_PATH="$sysroot/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

for command in \
    aarch64-linux-musl-gcc cargo rustc rustup rust-nm rust-objcopy gen_ksym \
    file nproc python3 readelf sha256sum; do
    command -v "$command" >/dev/null 2>&1 || fail "tool-missing-${command}"
done
echo "===${marker}-BEGIN run=${run_id} parallelism=system-default==="
echo "===${marker}-SOURCE-META-BEGIN==="
cat .tgoskits-source-meta
echo "===${marker}-SOURCE-META-END==="
rustc --version --verbose
cargo --version
rustup show active-toolchain
[ "$(rustc --print host-tuple)" = aarch64-unknown-linux-musl ] || fail toolchain-host-not-musl
echo "logical_cpus=$(nproc --all)"
echo "available_cpus=$(nproc)"
echo "cpu_affinity=$(sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' /proc/self/status)"
echo "native_target_tmpfs=${STARRY_SELFBUILD_NATIVE_TMPFS:-0}"
echo "cargo_incremental=$CARGO_INCREMENTAL"
proc_executable=$(readlink /proc/self/exe)
echo "proc_self_exe=$proc_executable"
[ -x "$proc_executable" ] || fail proc-self-exe-not-executable-inside-root

overall_start="$(date +%s)"
xtask_command=(cargo build -p tg-xtask)
printf '===%s-TG-XTASK-COMMAND===' "$marker"
printf ' %q' "${xtask_command[@]}"
printf '\n'
xtask_start="$(date +%s)"
xtask_compile_base="$(compile_unit_count)"
start_progress_sampler tg-xtask "$xtask_start" "$xtask_compile_base"
set +e
"${xtask_command[@]}"
xtask_rc="$?"
set -e
stop_progress_sampler
xtask_end="$(date +%s)"
xtask_elapsed="$((xtask_end - xtask_start))"
emit_progress tg-xtask "$xtask_start" "$xtask_compile_base"
echo "===${marker}-TG-XTASK-BUILD-END run=${run_id} rc=${xtask_rc} elapsed=${xtask_elapsed}==="
if [ "$xtask_rc" != "0" ]; then
    fail "tg-xtask-build rc=${xtask_rc}"
fi
[ -x "$xtask_bin" ] || fail tg-xtask-artifact-missing
file "$xtask_bin"
readelf -d "$xtask_bin" > "$run_dir/tg-xtask-dynamic.txt"
if grep -q 'Shared library: \[libc.so.6\]' "$run_dir/tg-xtask-dynamic.txt"; then
    fail tg-xtask-linked-glibc
fi
# Keep the executable, then free its intermediate objects before the kernel build.
cp "$xtask_bin" "$run_dir/tg-xtask"
xtask_bin="$run_dir/tg-xtask"
sha256sum "$xtask_bin" > "$run_dir/tg-xtask.sha256"
cat "$run_dir/tg-xtask.sha256"
if [ "${KEEP_TARGET:-0}" != "1" ]; then
    rm -rf "$target_dir"
    if [ "${STARRY_SELFBUILD_NATIVE_TMPFS:-0}" = 1 ]; then
        # The native cache is disposable. Release its RAM before starting the
        # kernel's memory-intensive build on the underlying ext4 directory.
        umount /work/targets
        printf 'released\n' > /work/native-target-released
    fi
fi

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
file "$artifact" > "$run_dir/artifact-type"
grep -q 'ELF 64-bit.*ARM aarch64' "$run_dir/artifact-type" || fail artifact-elf-architecture
readelf -h "$artifact" > "$run_dir/artifact-header"
grep -q 'Machine:.*AArch64' "$run_dir/artifact-header" || fail artifact-readelf-architecture

cp "$artifact" "$run_dir/starryos.elf"
cp "$artifact_bin" "$run_dir/starryos.bin"
cp .tgoskits-source-meta "$run_dir/source.meta"
printf '%s\n' "$xtask_elapsed" > "$run_dir/tg-xtask-elapsed-seconds"
printf '%s\n' "$elapsed" > "$run_dir/starry-build-elapsed-seconds"
printf '%s\n' "$total_elapsed" > "$run_dir/elapsed-seconds"
(
    cd "$run_dir"
    sha256sum tg-xtask starryos.elf starryos.bin source.meta > SHA256SUMS
)
cat "$run_dir/SHA256SUMS"
if [ "${KEEP_TARGET:-0}" != "1" ]; then
    case "$kernel_target_dir" in
        /work/targets/*)
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
