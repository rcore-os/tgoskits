#!/usr/bin/env python3
"""Regression checks for reproducible RT guest build paths."""

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
BUILD_ZEPHYR = (ROOT / "scripts/test/rt-partition/build-zephyr-periodic.sh").read_text()
BUILD_TRACE_LINUX = (
    ROOT / "scripts/test/rt-partition/build-linux-trace-kernel.sh"
).read_text()
NATIVE_RUNNER_PATH = ROOT / "scripts/test/rt-partition/run-native-zephyr.sh"
MATRIX_RUNNER = (ROOT / "scripts/test/rt-partition/run-cyclictest.sh").read_text()
FOUR_ARM_RUNNER = (ROOT / "scripts/test/rt-partition/run-four-arm-matrix.sh").read_text()
GUEST_SHARED_RUNNER_PATH = ROOT / "scripts/test/rt-partition/run-guest-shared-ab.sh"
GUEST_SHARED_THREE_ARM_RUNNER_PATH = (
    ROOT / "scripts/test/rt-partition/run-guest-shared-three-arm.sh"
)
PRIORITY_AB_RUNNER_PATH = (
    ROOT / "scripts/test/rt-partition/run-priority-scheduler-ab.sh"
)
TIMER_LOCK_AB_RUNNER_PATH = (
    ROOT / "scripts/test/rt-partition/run-timer-wheel-lock-ab.sh"
)
WFI_ISOLATION_RUNNER_PATH = (
    ROOT / "scripts/test/rt-partition/run-linux-wfi-isolation.sh"
)
EXIT_YIELD_AB_RUNNER_PATH = (
    ROOT / "scripts/test/rt-partition/run-vcpu-exit-yield-ab.sh"
)
TIMER_WORKER_AB_RUNNER_PATH = (
    ROOT / "scripts/test/rt-partition/run-timer-worker-priority-ab.sh"
)
ZEPHYR_MAIN = (ROOT / "scripts/test/zephyr-periodic/src/main.c").read_text()
RTTHREAD_MAIN = (
    ROOT / "scripts/test/net-dual-guest/rtthread-periodic/main.c"
).read_text()
BUILD_RTTHREAD = (
    ROOT / "scripts/test/net-dual-guest/build-rtthread-periodic.sh"
).read_text()


class RtBuildScriptsTest(unittest.TestCase):
    def test_serial_driver_handles_guest_completion_before_reattach(self):
        serial = (ROOT / "scripts/test/net-dual-guest/serial_console.py").read_text()
        self.assertIn("attach-if-needed", serial)
        self.assertIn("detach-if-attached", serial)

    def test_linux_measurement_can_hold_guest_for_console_drain(self):
        init = (ROOT / "scripts/test/rt-partition/rt-linux-init.sh").read_text()
        runner = MATRIX_RUNNER
        self.assertIn("rt_hold_after_complete", init)
        self.assertIn("RT_CYCLICTEST_HOLD_READY", init)
        self.assertIn("RT_CYCLICTEST_RELEASED", init)
        self.assertIn("RT_HOLD_AFTER_COMPLETE", runner)

    def test_guest_shared_runner_keeps_topology_constant_and_changes_scheduler_only(self):
        runner = GUEST_SHARED_RUNNER_PATH.read_text()
        self.assertIn("RT_SCENARIO=stress-guest-shared", runner)
        self.assertIn("RT_LINUX_PHYS_CPU_IDS=1,2", runner)
        self.assertIn("RT_ZEPHYR_PHYS_CPU_IDS=1", runner)
        self.assertIn("topology=linux-vcpu0->pcpu1,linux-vcpu1->pcpu2,zephyr-vcpu0->pcpu1", runner)
        self.assertIn('rr_features - {"rr-scheduler"}', runner)
        self.assertIn('fixed_features - {"rt-scheduler"}', runner)
        self.assertIn("--baseline-label guest-shared-rr", runner)
        self.assertIn("--modified-label guest-shared-fixed", runner)

    def test_guest_shared_three_arm_runner_keeps_three_scheduler_profiles(self):
        runner = GUEST_SHARED_THREE_ARM_RUNNER_PATH.read_text()
        self.assertIn("arms=rr,fixed,fp-rr", runner)
        self.assertIn("RT_SCENARIO=stress-guest-shared", runner)
        self.assertIn("RT_LINUX_PHYS_CPU_IDS=1,2", runner)
        self.assertIn("RT_ZEPHYR_PHYS_CPU_IDS=1", runner)
        self.assertIn("pairwise_compare rr-vs-fp-rr rr fp-rr", runner)

    def test_runner_supports_guest_shared_scenario_and_explicit_mappings(self):
        self.assertIn("stress-guest-shared)", MATRIX_RUNNER)
        self.assertIn('linux_phys_cpu_ids="${RT_LINUX_PHYS_CPU_IDS:-2,3}"', MATRIX_RUNNER)
        self.assertIn('zephyr_phys_cpu_ids="${RT_ZEPHYR_PHYS_CPU_IDS:-1}"', MATRIX_RUNNER)
        self.assertIn('linux_template_override="${RT_LINUX_TEMPLATE:-}"', MATRIX_RUNNER)
        self.assertIn('phys_cpu_ids = [{\', \'.join(phys_cpu_ids.split(\',\'))}]', MATRIX_RUNNER)
        self.assertIn("linux_phys_cpu_ids=%s", MATRIX_RUNNER)
        self.assertIn("zephyr_phys_cpu_ids=%s", MATRIX_RUNNER)

    def test_zephyr_build_normalizes_caller_supplied_relative_paths(self):
        self.assertIn('out_dir="$(realpath -m "$out_dir")"', BUILD_ZEPHYR)
        self.assertIn('build_dir="$(realpath -m "$build_dir")"', BUILD_ZEPHYR)

    def test_zephyr_build_accepts_a_board_guest_overlay(self):
        self.assertIn('extra_overlay="${ZEPHYR_EXTRA_OVERLAY:-}"', BUILD_ZEPHYR)
        self.assertIn('overlay_files=("$overlay")', BUILD_ZEPHYR)
        self.assertIn('overlay_files+=("$(realpath "$extra_overlay")")', BUILD_ZEPHYR)
        self.assertIn('IFS=";"; printf "%s" "${overlay_files[*]}"', BUILD_ZEPHYR)
        self.assertIn('printf \'extra_overlay=%s\\n\'', BUILD_ZEPHYR)

    def test_zephyr_build_records_the_uart_start_gate(self):
        self.assertIn('start_gated="${ZEPHYR_START_GATED:-1}"', BUILD_ZEPHYR)
        self.assertIn('-DRT_START_GATED="$start_gated"', BUILD_ZEPHYR)
        self.assertIn('-DRT_DUMP_GATED="$dump_gated"', BUILD_ZEPHYR)
        self.assertIn('start_delay_ms="${ZEPHYR_START_DELAY_MS:-0}"', BUILD_ZEPHYR)
        self.assertIn('-DRT_START_DELAY_MS="$start_delay_ms"', BUILD_ZEPHYR)
        self.assertIn("start_gated=%s", BUILD_ZEPHYR)
        self.assertIn("PERIODIC LATENCY READY", ZEPHYR_MAIN)
        self.assertIn("k_sleep(K_MSEC(1));", ZEPHYR_MAIN)
        self.assertIn("PERIODIC LATENCY SETTLE", ZEPHYR_MAIN)
        self.assertIn("uart_poll_in", ZEPHYR_MAIN)

    def test_zephyr_sample_count_is_a_build_and_runner_contract(self):
        self.assertIn("#ifndef RT_SAMPLE_COUNT", ZEPHYR_MAIN)
        self.assertIn("#define SAMPLE_COUNT RT_SAMPLE_COUNT", ZEPHYR_MAIN)
        self.assertIn('sample_count="${ZEPHYR_SAMPLE_COUNT:-300}"', BUILD_ZEPHYR)
        self.assertIn('-DRT_SAMPLE_COUNT="$sample_count"', BUILD_ZEPHYR)
        self.assertIn('printf \'sample_count=%s\\n\' "$sample_count"', BUILD_ZEPHYR)
        self.assertIn(
            'zephyr_sample_count_expected="${RT_ZEPHYR_SAMPLE_COUNT:-300}"',
            MATRIX_RUNNER,
        )
        self.assertIn("expected_samples = int(sys.argv[4])", MATRIX_RUNNER)
        self.assertIn("expected_samples = int(sys.argv[13])", MATRIX_RUNNER)
        self.assertIn("zephyr_sample_count=%s", MATRIX_RUNNER)

    def test_rtthread_period_and_sample_count_remain_build_configurable(self):
        self.assertIn("#ifndef PERIOD_MS", RTTHREAD_MAIN)
        self.assertIn("#ifndef SAMPLE_COUNT", RTTHREAD_MAIN)
        self.assertIn('period_ms="${RTTHREAD_PERIOD_MS:-10}"', BUILD_RTTHREAD)
        self.assertIn('sample_count="${RTTHREAD_SAMPLE_COUNT:-300}"', BUILD_RTTHREAD)
        self.assertIn("s/#define PERIOD_MS 10/#define PERIOD_MS $period_ms/", BUILD_RTTHREAD)
        self.assertIn("s/#define SAMPLE_COUNT 300/#define SAMPLE_COUNT $sample_count/", BUILD_RTTHREAD)

    def test_rtthread_counter_conversion_avoids_long_run_multiply_overflow(self):
        self.assertIn("cycles / freq", RTTHREAD_MAIN)
        self.assertIn("cycles % freq", RTTHREAD_MAIN)
        self.assertNotIn(
            "cycles * UINT64_C(1000000000)",
            RTTHREAD_MAIN,
        )

    def test_matrix_runner_rejects_tracked_dirty_sources_by_default(self):
        self.assertIn('allow_dirty="${RT_ALLOW_DIRTY:-0}"', MATRIX_RUNNER)
        self.assertIn("status --porcelain --untracked-files=no", MATRIX_RUNNER)
        self.assertIn("RT_ALLOW_DIRTY must be 0 or 1", MATRIX_RUNNER)
        self.assertIn("tracked_dirty=%s", MATRIX_RUNNER)
        self.assertIn("untracked_count=%s", MATRIX_RUNNER)

    def test_four_arm_runner_separates_topology_and_scheduler(self):
        self.assertIn("arms=(shared-rr shared-fixed partition-rr partition-fixed)", FOUR_ARM_RUNNER)
        self.assertIn("RT_DEDICATED_CPUS_OVERRIDE=\"$dedicated\"", FOUR_ARM_RUNNER)
        self.assertIn("RT_ZEPHYR_SAMPLE_COUNT=\"$sample_count\"", FOUR_ARM_RUNNER)
        self.assertIn("pairwise_compare shared-scheduler shared-rr shared-fixed", FOUR_ARM_RUNNER)
        self.assertIn("pairwise_compare partition-effect shared-rr partition-rr", FOUR_ARM_RUNNER)
        self.assertIn("pairwise_compare partitioned-scheduler partition-rr partition-fixed", FOUR_ARM_RUNNER)
        self.assertIn("pairwise_compare shared-vs-partition shared-fixed partition-fixed", FOUR_ARM_RUNNER)

    def test_native_zephyr_runner_archives_complete_evidence(self):
        runner = NATIVE_RUNNER_PATH.read_text()
        self.assertIn("PERIODIC LATENCY COMPLETE samples=300", runner)
        self.assertIn("expected 300 native Zephyr samples", runner)
        self.assertIn("rt_latency_stats.py", runner)
        self.assertIn("sha256sums", runner)
        self.assertIn("-cpu cortex-a72", runner)
        self.assertNotIn("-icount", runner)
        self.assertIn("timing_model=wall-clock TCG", runner)
        self.assertIn("(( linked_base == 0x40000000 ))", runner)
        self.assertIn('input_bin="${input_dir}/zephyr-periodic.bin"', runner)
        self.assertIn('actual_sha="$(sha256sum "$input_bin"', runner)
        self.assertIn('[[ "$start_gated" == "0" ]]', runner)

    def test_zephyr_sampler_defers_console_output_until_sampling_finishes(self):
        main_body = ZEPHYR_MAIN.split("int main(void)", 1)[1]
        sample_loop = main_body.split(
            "for (int64_t sequence = 0; sequence < SAMPLE_COUNT; sequence++) {", 1
        )[1].split("\n\t}", 1)[0]
        self.assertNotIn("printk", sample_loop)
        self.assertIn("static struct latency_sample samples[SAMPLE_COUNT]", ZEPHYR_MAIN)
        self.assertIn("print_samples(samples)", ZEPHYR_MAIN)

    def test_matrix_runner_hashes_archived_build_inputs(self):
        self.assertIn(
            'linux_image="${RT_LINUX_KERNEL_OVERRIDE:-${repo_root}/tmp/rt-partition/linux-qemu}"',
            MATRIX_RUNNER,
        )
        self.assertIn('cp "$linux_image" "$out_dir/linux-qemu"', MATRIX_RUNNER)
        self.assertIn('printf \'linux_kernel=%s\\n\' "$linux_image"', MATRIX_RUNNER)
        self.assertIn('lines[index] = f\'kernel_path = "{linux_image}"\'', MATRIX_RUNNER)
        self.assertIn('cp "$work/rt-linux-initramfs.cpio.gz" "$out_dir/"', MATRIX_RUNNER)
        self.assertIn('linux_trace="${RT_LINUX_TRACE:-disabled}"', MATRIX_RUNNER)
        self.assertIn('linux_virtual_timer_only="${RT_LINUX_VIRTUAL_TIMER_ONLY:-0}"', MATRIX_RUNNER)
        self.assertIn('linux_wfi_policy="${RT_LINUX_WFI_POLICY:-auto}"', MATRIX_RUNNER)
        self.assertIn('dedicated_cpus_override="${RT_DEDICATED_CPUS_OVERRIDE:-}"', MATRIX_RUNNER)
        self.assertIn('aarch64_virtual_timer_only = ', MATRIX_RUNNER)
        self.assertIn('aarch64_wfi_policy = ', MATRIX_RUNNER)
        self.assertIn('linux-ftrace.txt', MATRIX_RUNNER)
        self.assertIn('linux-ftrace-latency.csv', MATRIX_RUNNER)
        self.assertIn('linux-ftrace-latency-summary.txt', MATRIX_RUNNER)
        self.assertIn('linux-timerlat.txt', MATRIX_RUNNER)
        self.assertIn('linux-timerlat-latency.csv', MATRIX_RUNNER)
        self.assertIn('linux-timerlat-latency-summary.txt', MATRIX_RUNNER)
        self.assertIn('RT_FTRACE_DUMP_READY encoding=gzip-base64', MATRIX_RUNNER)
        self.assertIn('cmd dump', MATRIX_RUNNER)
        self.assertIn('gzip.decompress(base64.b64decode', MATRIX_RUNNER)
        self.assertIn('linux-ftrace-latency.py', MATRIX_RUNNER)
        self.assertIn('linux-timerlat-latency.py', MATRIX_RUNNER)
        self.assertIn(
            'cp "$zephyr_image" "$out_dir/zephyr-periodic.bin"', MATRIX_RUNNER
        )
        self.assertIn(
            'zephyr_image="${RT_ZEPHYR_IMAGE:-${work}/zephyr-periodic.bin}"',
            MATRIX_RUNNER,
        )
        self.assertIn('cp "$axvisor_bin" "$out_dir/"', MATRIX_RUNNER)
        hash_block = MATRIX_RUNNER.rsplit("sha256sum", 1)[1]
        self.assertNotIn("$work", hash_block)
        self.assertNotIn("$axvisor_bin", hash_block)

    def test_trace_linux_build_preserves_scheduling_config(self):
        self.assertIn("scripts/extract-ikconfig", BUILD_TRACE_LINUX)
        self.assertIn('command -v "${cross_prefix}gcc"', BUILD_TRACE_LINUX)
        self.assertIn("--enable OSNOISE_TRACER", BUILD_TRACE_LINUX)
        self.assertIn("--enable TIMERLAT_TRACER", BUILD_TRACE_LINUX)
        self.assertIn('assert_config_unchanged PREEMPT', BUILD_TRACE_LINUX)
        self.assertIn('assert_config_unchanged NO_HZ_FULL', BUILD_TRACE_LINUX)
        self.assertIn('assert_config_unchanged SHADOW_CALL_STACK', BUILD_TRACE_LINUX)
        self.assertIn('assert_config_unchanged INIT_STACK_ALL_ZERO', BUILD_TRACE_LINUX)
        self.assertIn("trace-kernel.manifest", BUILD_TRACE_LINUX)

    def test_matrix_runner_allows_stress_results_to_drain_after_cyclictest(self):
        self.assertIn(
            'result_drain_timeout="${RT_RESULT_DRAIN_TIMEOUT_SEC:-180}"',
            MATRIX_RUNNER,
        )
        self.assertIn(
            "expect ${result_drain_timeout} RT_INIT_DONE scenario=${scenario}",
            MATRIX_RUNNER,
        )
        self.assertNotIn("expect 30 RT_INIT_DONE", MATRIX_RUNNER)

    def test_matrix_runner_bounds_qmp_shutdown_hangs(self):
        self.assertIn(
            'qemu_exit_grace_sec="${RT_QEMU_EXIT_GRACE_SEC:-10}"',
            MATRIX_RUNNER,
        )
        self.assertIn('wait_for_run_exit "$qemu_exit_grace_sec"', MATRIX_RUNNER)
        self.assertIn('kill -TERM "$run_pid"', MATRIX_RUNNER)
        self.assertIn('kill -KILL "$run_pid"', MATRIX_RUNNER)
        self.assertIn('qemu_shutdown="forced-term"', MATRIX_RUNNER)
        self.assertIn('qemu_shutdown="qmp"', MATRIX_RUNNER)

    def test_timer_lock_ab_runner_is_host_only_and_single_variable(self):
        runner = TIMER_LOCK_AB_RUNNER_PATH.read_text()
        self.assertIn('"-smp", "4"', runner)
        self.assertIn("rt timer-storm --cpus 0xe", runner)
        self.assertIn("host_only=1", runner)
        self.assertIn("RT_TIMER_STORM_COMPLETE", runner)
        self.assertIn("summarize-timer-wheel-ab.py", runner)
        self.assertNotIn("--vmconfigs", runner)

    def test_priority_ab_runner_counterbalances_order_and_collects_diagnostics(self):
        runner = PRIORITY_AB_RUNNER_PATH.read_text()
        self.assertIn('repeats="${RT_PRIORITY_AB_REPEATS:-4}"', runner)
        self.assertIn('order="${RT_PRIORITY_AB_ORDER:-counterbalanced}"', runner)
        self.assertIn("repeats % 2 == 0", runner)
        self.assertIn('for run_number in $(seq 1 "$repeats")', runner)
        self.assertIn("printf -v run_id 'run-%02d'", runner)
        self.assertIn("run_number % 2", runner)
        self.assertIn('sequence+=("rr")', runner)
        self.assertIn('sequence+=("fixed-priority")', runner)
        self.assertIn("sequence=${sequence_csv}", runner)
        self.assertIn("RT_RUNTIME_DIAGNOSTICS=1", runner)
        self.assertIn("compare-rt-runs.py", runner)
        self.assertIn("deduplicate_archive", runner)
        self.assertIn('cmp -s "$canonical" "$candidate"', runner)
        self.assertIn('ln -f "$canonical" "$candidate"', runner)

    def test_wfi_isolation_runner_defines_three_legal_single_variable_cells(self):
        runner = WFI_ISOLATION_RUNNER_PATH.read_text()
        self.assertIn("cntp-trap cntv-trap cntv-passthrough", runner)
        self.assertIn('run_cell "$cell" "$run_id" 0 trap', runner)
        self.assertIn('run_cell "$cell" "$run_id" 1 trap', runner)
        self.assertIn('run_cell "$cell" "$run_id" 1 passthrough', runner)
        self.assertNotIn('run_cell "$cell" "$run_id" 0 passthrough', runner)
        self.assertIn("timer-contract-comparison.txt", runner)
        self.assertIn("wfi-path-comparison.txt", runner)
        self.assertIn("legacy-coupled-comparison.txt", runner)
        self.assertIn("RT_RUNTIME_DIAGNOSTICS=1", runner)
        self.assertIn("linux-qemu-trace", runner)
        self.assertIn('RT_LINUX_KERNEL_OVERRIDE="$linux_kernel"', runner)
        self.assertIn("RT_DEDICATED_CPUS_OVERRIDE=1,2,3", runner)
        self.assertIn("dedicated_cpus=1,2,3", runner)
        dedup_block = runner.split("deduplicate_archive()", 1)[1].split(
            "run_cell()", 1
        )[0]
        self.assertNotIn("axvisor.bin", dedup_block)
        self.assertIn("linux-qemu rt-linux-initramfs.cpio.gz", dedup_block)

    def test_exit_yield_runner_is_single_variable_and_counterbalanced(self):
        runner = EXIT_YIELD_AB_RUNNER_PATH.read_text()
        self.assertIn("no-vcpu-exit-yield", runner)
        self.assertIn("differ outside features", runner)
        self.assertIn("modified board must add only no-vcpu-exit-yield", runner)
        self.assertIn("run_number % 2", runner)
        self.assertIn("RT_LINUX_VIRTUAL_TIMER_ONLY=1", runner)
        self.assertIn("RT_LINUX_WFI_POLICY=trap", runner)
        self.assertIn("RT_DEDICATED_CPUS_OVERRIDE=1,2,3", runner)
        self.assertIn("RT_RUNTIME_DIAGNOSTICS=1", runner)
        self.assertIn("post-vmexit-yield", runner)
        self.assertIn("no-post-vmexit-yield", runner)
        self.assertIn("summarize-vcpu-exit-yield-ab.py", runner)
        self.assertIn("mechanism-comparison.txt", runner)

    def test_timer_worker_runner_is_single_variable_bounded_and_counterbalanced(self):
        runner = TIMER_WORKER_AB_RUNNER_PATH.read_text()
        self.assertIn("timer-worker-priority-boost", runner)
        self.assertIn("differ outside features", runner)
        self.assertIn(
            "modified board must add only timer-worker-priority-boost", runner
        )
        self.assertIn("run_number % 2", runner)
        self.assertIn("RT_TIMER_WORKER_AB_START_CELL", runner)
        self.assertIn("modified_event_budget_per_wake=1", runner)
        self.assertIn("RT_LINUX_VIRTUAL_TIMER_ONLY=1", runner)
        self.assertIn("RT_LINUX_WFI_POLICY=trap", runner)
        self.assertIn("RT_DEDICATED_CPUS_OVERRIDE=1,2,3", runner)
        self.assertIn("RT_RUNTIME_DIAGNOSTICS=1", runner)
        self.assertIn("timer-worker-priority-89", runner)
        self.assertIn("bounded-timer-worker-priority-91", runner)


if __name__ == "__main__":
    unittest.main()
