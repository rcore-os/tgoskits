#!/usr/bin/env bash

set -euo pipefail

benchmark_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
builder=$benchmark_dir/build-starry-rootfs.sh
kernel_builder=$benchmark_dir/build-starry-kernel.sh
soak_builder=$benchmark_dir/prepare-starry-soak.sh
noise_builder=$benchmark_dir/build-aarch64-noise-guest.sh
stage_runner=$benchmark_dir/stage-starry-board.sh
harvest_runner=$benchmark_dir/harvest-starry-board.sh
guest_runner=$benchmark_dir/guest/starry_rt_compat_run.sh
capture_runner=$benchmark_dir/guest/starry_rt_capture_run.sh
irq_analyzer=$benchmark_dir/analyze_irq_trace.py
config_dir=$benchmark_dir/config
noise_source=$benchmark_dir/guest/aarch64_rt_noise.S
noise_shared_config=$config_dir/aarch64-rt-noise-shared.toml
noise_partitioned_config=$config_dir/aarch64-rt-noise-partitioned.toml
host_noise_shared_build=$config_dir/axvisor-orangepi-5-plus-starry-host-noise-shared.toml
host_noise_partitioned_build=$config_dir/axvisor-orangepi-5-plus-starry-host-noise-partitioned.toml
host_noise_formal_shared_build=$config_dir/axvisor-orangepi-5-plus-starry-host-noise-formal-shared.toml
host_noise_formal_partitioned_build=$config_dir/axvisor-orangepi-5-plus-starry-host-noise-formal-partitioned.toml
host_noise_soak_shared_build=$config_dir/axvisor-orangepi-5-plus-starry-host-noise-soak-shared.toml
host_noise_soak_partitioned_build=$config_dir/axvisor-orangepi-5-plus-starry-host-noise-soak-partitioned.toml
host_noise_source=$benchmark_dir/../../../os/axvisor/src/host_noise.rs
host_noise_shared_board=$config_dir/board-orangepi-5-plus-starry-host-noise-shared.toml
host_noise_partitioned_board=$config_dir/board-orangepi-5-plus-starry-host-noise-partitioned.toml
host_noise_formal_shared_board=$config_dir/board-orangepi-5-plus-starry-host-noise-formal-shared.toml
host_noise_formal_partitioned_board=$config_dir/board-orangepi-5-plus-starry-host-noise-formal-partitioned.toml
host_noise_soak_shared_board=$config_dir/board-orangepi-5-plus-starry-host-noise-soak-shared.toml
host_noise_soak_partitioned_board=$config_dir/board-orangepi-5-plus-starry-host-noise-soak-partitioned.toml

fail() {
    echo "test_starry_runner: $*" >&2
    exit 1
}

bash -n "$builder"
bash -n "$kernel_builder"
bash -n "$soak_builder"
bash -n "$noise_builder"
bash -n "$stage_runner"
bash -n "$harvest_runner"
sh -n "$guest_runner"
sh -n "$capture_runner"
python3 -m py_compile "$irq_analyzer"

grep -q 'mrs.*cntvct_el0' "$noise_source" || \
    fail "noise guest must use the guest virtual counter for a bounded run"
grep -q 'AXVISOR_RT_NOISE_COUNTER_HZ=24000000' "$noise_builder" || \
    fail "RK3588 noise builds must pin the architected counter to 24 MHz"
grep -q 'AXVISOR_RT_NOISE_COUNTER_HZ' "$noise_source" || \
    fail "noise guest duration must use the build-time counter-frequency contract"
if grep -q 'cntfrq_el0' "$noise_source"; then
    fail "noise guest must not trust the uninitialized CNTFRQ_EL0 value"
fi
grep -q '0x8400.*0008' "$noise_source" || \
    fail "noise guest must terminate through PSCI SYSTEM_OFF"
grep -q 'phys_cpu_sets = \[0x2\]' "$noise_shared_config" || \
    fail "shared noise vCPU must be pinned to StarryOS vCPU0's pCPU1"
grep -q 'phys_cpu_sets = \[0x8\]' "$noise_partitioned_config" || \
    fail "partitioned noise vCPU must be pinned to isolated pCPU3"
for noise_config in "$noise_shared_config" "$noise_partitioned_config"; do
    grep -q 'dedicated_cpus = false' "$noise_config" || \
        fail "$(basename "$noise_config") must remain a shared VM"
    grep -q 'interrupt_mode = "no_irq"' "$noise_config" || \
        fail "noise guest must not add an interrupt backend to the measured path"
done
normalized_shared=$(sed 's/^phys_cpu_sets = .*/phys_cpu_sets = [NORMALIZED]/' "$noise_shared_config")
normalized_partitioned=$(sed 's/^phys_cpu_sets = .*/phys_cpu_sets = [NORMALIZED]/' "$noise_partitioned_config")
[[ "$normalized_shared" == "$normalized_partitioned" ]] || \
    fail "noise profiles must differ only in physical CPU placement"
for build_profile in shared partitioned; do
    base_build_config=$config_dir/axvisor-orangepi-5-plus-starry-$build_profile.toml
    noise_build_config=$config_dir/axvisor-orangepi-5-plus-starry-noise-$build_profile.toml
    if grep -q 'aarch64-rt-noise' "$base_build_config"; then
        fail "$(basename "$base_build_config") must remain a single-guest baseline"
    fi
    grep -q "aarch64-rt-noise-$build_profile.toml" "$noise_build_config" || \
        fail "$(basename "$noise_build_config") must include its singleton noise placement"
    grep -q 'ax-std/sched-rr' "$noise_build_config" || \
        fail "$(basename "$noise_build_config") must preempt CPU-bound shared vCPUs"
done
grep -q -- '--noise-guest' "$stage_runner" || \
    fail "board staging must accept the reproducible noise guest artifact"

grep -q 'cpu = 1' "$host_noise_shared_build" || \
    fail "shared host noise must contend with StarryOS vCPU0 on pCPU1"
grep -q 'cpu = 3' "$host_noise_partitioned_build" || \
    fail "partitioned host noise must run away from StarryOS vCPU0 on pCPU3"
for host_noise_build in "$host_noise_shared_build" "$host_noise_partitioned_build"; do
    grep -q 'max_duration_ms = 180000' "$host_noise_build" || \
        fail "$(basename "$host_noise_build") must use the common bounded duration"
    grep -q 'ax-std/sched-rr' "$host_noise_build" || \
        fail "$(basename "$host_noise_build") must preempt the CPU-bound host task"
    if grep -q 'aarch64-rt-noise' "$host_noise_build"; then
        fail "$(basename "$host_noise_build") must use one StarryOS guest plus host noise"
    fi
done
for host_noise_formal_build in \
    "$host_noise_formal_shared_build" \
    "$host_noise_formal_partitioned_build"; do
    grep -q 'max_duration_ms = 600000' "$host_noise_formal_build" || \
        fail "$(basename "$host_noise_formal_build") must cover the 10,000-sample formal run"
    grep -q 'ax-std/sched-rr' "$host_noise_formal_build" || \
        fail "$(basename "$host_noise_formal_build") must retain round-robin host scheduling"
done
for host_noise_soak_build in \
    "$host_noise_soak_shared_build" \
    "$host_noise_soak_partitioned_build"; do
    grep -q 'max_duration_ms = 3600000' "$host_noise_soak_build" || \
        fail "$(basename "$host_noise_soak_build") must allow the bounded 30-minute soak"
    grep -q '"rt-trace-soak"' "$host_noise_soak_build" || \
        fail "$(basename "$host_noise_soak_build") must enable the enlarged host trace"
    grep -q 'ax-std/sched-rr' "$host_noise_soak_build" || \
        fail "$(basename "$host_noise_soak_build") must retain round-robin host scheduling"
done
grep -q 'AXVISOR_RT_HOST_NOISE schema=1' "$host_noise_source" || \
    fail "host noise must emit a machine-readable persisted evidence record"
for host_noise_board in "$host_noise_shared_board" "$host_noise_partitioned_board"; do
    grep -q '^shell_init_cmd = "rs"' "$host_noise_board" || \
        fail "$(basename "$host_noise_board") must request the snapshot-sync shell alias"
    grep -q 'stop_reason=max-duration' "$host_noise_board" || \
        fail "$(basename "$host_noise_board") must reject an expired host-noise task"
    success_block=$(sed -n '/^success_regex = \[/,/^\]/p' "$host_noise_board")
    success_count=$(printf '%s\n' "$success_block" | grep -c '^  "')
    [[ "$success_count" -eq 1 ]] || \
        fail "$(basename "$host_noise_board") must have one terminal success condition"
    printf '%s\n' "$success_block" | grep -q 'AXVISOR_SNAPSHOT_SYNC_OK' || \
        fail "$(basename "$host_noise_board") must succeed only after the snapshot is synced"
done
for host_noise_formal_board in \
    "$host_noise_formal_shared_board" \
    "$host_noise_formal_partitioned_board"; do
    grep -q '^timeout = 1200' "$host_noise_formal_board" || \
        fail "$(basename "$host_noise_formal_board") must allow the bounded formal run and restore"
    grep -q 'stop_reason=max-duration' "$host_noise_formal_board" || \
        fail "$(basename "$host_noise_formal_board") must reject an expired formal host-noise task"
    success_block=$(sed -n '/^success_regex = \[/,/^\]/p' "$host_noise_formal_board")
    success_count=$(printf '%s\n' "$success_block" | grep -c '^  "')
    [[ "$success_count" -eq 1 ]] || \
        fail "$(basename "$host_noise_formal_board") must have one terminal success condition"
    printf '%s\n' "$success_block" | grep -q 'AXVISOR_SNAPSHOT_SYNC_OK' || \
        fail "$(basename "$host_noise_formal_board") must succeed only after snapshot sync"
done
for host_noise_soak_board in \
    "$host_noise_soak_shared_board" \
    "$host_noise_soak_partitioned_board"; do
    grep -q '^timeout = 4500' "$host_noise_soak_board" || \
        fail "$(basename "$host_noise_soak_board") must include trace persistence and Linux restore"
    grep -q 'stop_reason=max-duration' "$host_noise_soak_board" || \
        fail "$(basename "$host_noise_soak_board") must reject an expired soak host-noise task"
done
grep -q 'ORANGEPI_RT_EXPECTED_HOST_NOISE_PCPU' "$harvest_runner" || \
    fail "harvest must expose an explicit expected host-noise placement"
grep -q -- '--expected-host-noise-pcpu' "$harvest_runner" || \
    fail "harvest must pass the host-noise placement contract to analysis"

grep -q '"rt-irq-trace"' "$config_dir/starry-aarch64-rt.toml" || \
    fail "StarryOS RT kernel must enable the guest IRQ trace feature"
grep -q '"rt-irq-trace-soak"' "$config_dir/starry-aarch64-rt-soak.toml" || \
    fail "StarryOS soak kernel must enable the enlarged guest IRQ trace"
grep -q 'STARRY_RT_CONFIG' "$kernel_builder" || \
    fail "StarryOS RT kernel builder must accept an explicit soak config"
grep -q '^iterations=10000$' "$soak_builder" || \
    fail "soak preparation must retain the formal 10,000 samples per metric"
grep -q '^period_us=90000$' "$soak_builder" || \
    fail "soak preparation must use two 15-minute timed phases"
grep -q '^minimum_duration_seconds=1800$' "$soak_builder" || \
    fail "soak preparation must enforce a 30-minute nominal timed window"
grep -q 'starry-rt-soak-rootfs.img' "$soak_builder" || \
    fail "soak preparation must produce a distinct immutable rootfs artifact"
grep -Fq 'rustup run "$toolchain" llvm-objcopy --strip-all -O binary "$built_elf" "$built_kernel"' \
    "$kernel_builder" || \
    fail "StarryOS RT kernel build must materialize a fresh BIN from the clean-tree ELF"
grep -q 'tmp/axvisor-rt/starryos-rt.bin' "$stage_runner" || \
    fail "board staging must default to the trace-enabled StarryOS kernel"

grep -q 'run_metric clock-affinity periodic_jitter 0' "$guest_runner" || \
    fail "absolute sleep and affinity must be isolated from SCHED_FIFO"
grep -q 'run_metric sched-fifo periodic_jitter "$fifo_priority"' "$guest_runner" || \
    fail "SCHED_FIFO must have its own compatibility phase"
grep -q 'run_metric pthread-eventfd dispatch_latency "$fifo_priority"' "$guest_runner" || \
    fail "pthread/eventfd dispatch phase is missing"
grep -q 'run_metric timerfd emulated_irq_response "$fifo_priority"' "$guest_runner" || \
    fail "timerfd phase is missing"
grep -q 'AXVISOR_RT_STARRY_COMPAT_COMPLETE schema=1' "$guest_runner" || \
    fail "loss-tolerant completion marker is missing"
grep -q 'cpu_num = 2' "$config_dir/starry-orangepi-5-plus-smp2-dedicated.toml" || \
    fail "StarryOS RT guest must expose two vCPUs"
grep -q 'dedicated_cpus = true' "$config_dir/starry-orangepi-5-plus-smp2-dedicated.toml" || \
    fail "compatibility config must use the known-good dedicated mapping"
grep -q '>"$metric_log"' "$capture_runner" || \
    fail "capture mode must buffer probe output instead of streaming samples over UART"
grep -q '"$BB" cat "$metric_log" >>"$RAW_LOG"' "$capture_runner" || \
    fail "capture mode must append validated metric output to the raw file"
grep -q '"$BB" sync || fatal final-sync' "$capture_runner" || \
    fail "capture mode must sync raw data before publishing its hash"
grep -q '/proc/axvisor_rt_timer_trace' "$capture_runner" || \
    fail "capture mode must export the direct guest timer IRQ trace"
grep -q 'guest-timer-trace.log.gz' "$capture_runner" || \
    fail "capture mode must retain a compressed guest IRQ trace"
if grep -Fq '"$BB" rm -f "$metric_log"' "$capture_runner"; then
    fail "capture mode must retain metric logs to avoid live-ext4 orphan records"
fi
if grep -Fq '"$BB" rm -f "$stress_log"' "$capture_runner"; then
    fail "capture mode must retain the stress log to avoid live-ext4 orphan records"
fi
grep -q 'phys_cpu_sets = \[0x2, 0x4\]' \
    "$config_dir/starry-orangepi-5-plus-smp2-shared.toml" || \
    fail "shared baseline must keep timer-owning vCPUs pinned while sharing their pCPUs"
grep -q 'phys_cpu_sets = \[0x2, 0x4\]' \
    "$config_dir/starry-orangepi-5-plus-smp2-partitioned.toml" || \
    fail "partitioned profile must isolate its two vCPUs"
for soak_profile in shared partitioned; do
    soak_vm=$config_dir/starry-orangepi-5-plus-smp2-soak-$soak_profile.toml
    grep -q 'memory_regions = ' "$soak_vm" || \
        fail "$(basename "$soak_vm") must declare guest memory"
    grep -q '\[0x8000_0000, 0x2000_0000, 0x7, 0\]' "$soak_vm" || \
        fail "$(basename "$soak_vm") must provide 512 MiB for the enlarged trace export"
    grep -q 'starry-rt-soak-rootfs.img' "$soak_vm" || \
        fail "$(basename "$soak_vm") must use the dedicated soak result image"
done
grep -q 'shell_init_cmd = "rs"' \
    "$config_dir/board-orangepi-5-plus-starry-shared.toml" || \
    fail "shared capture must use the UART-safe snapshot command"
for board_config in \
    "$config_dir/board-orangepi-5-plus-starry-shared.toml" \
    "$config_dir/board-orangepi-5-plus-starry-partitioned.toml"; do
    grep -q 'ESR_EL2:' "$board_config" || \
        fail "$(basename "$board_config") must stop on the first current-EL syndrome"
done
grep -q 'RT_SNAPSHOT_OUTPUT_PATH.*"/home/rt"' \
    "$benchmark_dir/../../../os/axvisor/src/shell/command/host.rs" || \
    fail "UART-safe RT snapshot alias must use the approved fixed output path"
grep -q 'cargo xtask board connect -b "$board_type"' "$stage_runner" || \
    fail "board staging must hold a board-service lease"
grep -q 'sha256sum -c .rt-stage.sha256' "$stage_runner" || \
    fail "board staging must verify the remote artifact manifest"
grep -q 'e2fsck_path.*-fn.*"$result_image"' "$harvest_runner" || \
    fail "board harvest must read-only check the snapshot filesystem"
grep -q 'cat "$fsck_log" >&2' "$harvest_runner" || \
    fail "board harvest must preserve e2fsck diagnostics on failure"
grep -q 'cp --reflink=auto.*"$result_image".*"$repaired_image"' "$harvest_runner" || \
    fail "unclean snapshots must be repaired only through a disposable copy"
grep -q 'cmp -s.*"$temporary".*"$repaired_raw"' "$harvest_runner" || \
    fail "direct and repaired-copy raw evidence must match byte for byte"
grep -q 'debugfs_path.*dump.*$guest_raw_path' "$harvest_runner" || \
    fail "board harvest must extract the guest raw log from the snapshot"
grep -q 'guest-timer-trace.log.gz' "$harvest_runner" || \
    fail "board harvest must extract the lossless guest IRQ trace"
grep -q '\.host.log' "$harvest_runner" || \
    fail "board harvest must collect the independent AxVisor host trace"
grep -q 'analyze_irq_trace.py' "$harvest_runner" || \
    fail "board harvest must validate direct IRQ and host accounting evidence"
grep -q 'analyze_starry_board.py' "$harvest_runner" || \
    fail "board harvest must validate extracted raw evidence"

set +e
output=$(
    "$builder" --iterations 0 2>&1
)
status=$?
set -e
[[ "$status" -eq 2 ]] || fail "invalid iteration count returned $status: $output"
[[ "$output" == *'iterations/period-us must be positive'* ]] || \
    fail "invalid iteration count did not report its contract: $output"

echo "test_starry_runner: PASS"
