#!/usr/bin/env python3
"""Regression checks for the two-vCPU RT Linux workload topology."""

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
RUNNER = (ROOT / "scripts/test/rt-partition/run-cyclictest.sh").read_text()
INIT = (ROOT / "scripts/test/rt-partition/rt-linux-init.sh").read_text()
P1_RUNNER = (ROOT / "scripts/test/rt-partition/run-p1-comparison.sh").read_text()
GUEST_SHARED_RUNNER = (
    ROOT / "scripts/test/rt-partition/run-guest-shared-ab.sh"
).read_text()
RUNNER_STEPS = RUNNER.split('cat > "$steps" <<EOF', 1)[1].split("\nEOF", 1)[0]


class RtLinuxAffinityTest(unittest.TestCase):
    def test_p1_runner_interleaves_official_baseline_and_dedicated_runs(self):
        baseline = P1_RUNNER.index("P1_RUN_START variant=baseline")
        modified = P1_RUNNER.index("P1_RUN_START variant=modified")
        self.assertLess(baseline, modified)
        self.assertIn('RT_SOURCE_ROOT="$baseline_root"', P1_RUNNER)
        self.assertIn("RT_SCENARIO=stress-noiso", P1_RUNNER)
        self.assertIn("RT_SCENARIO=stress-dedicated", P1_RUNNER)
        self.assertIn('RT_P1_REPEATS:-3', P1_RUNNER)
        self.assertIn('RT_P1_MAX_ATTEMPTS:-3', P1_RUNNER)
        self.assertIn("run_with_retries", P1_RUNNER)
        self.assertIn('compare-rt-runs.py', P1_RUNNER)

    def test_p1_runner_requires_identical_burner_implementations(self):
        self.assertIn("cmp -s", P1_RUNNER)
        self.assertIn("do not use the same RT burner implementation", P1_RUNNER)

    def test_rt_partition_only_silences_the_zephyr_host_cpu(self):
        stress_rt = RUNNER.split("stress-rt)", 1)[1].split(";;", 1)[0]
        self.assertIn('dedicated_cpus="1"', stress_rt)

    def test_matrix_has_a_single_variable_dedicated_virtualized_scenario(self):
        scenario = RUNNER.split("stress-dedicated)", 1)[1].split(";;", 1)[0]
        self.assertIn('dedicated_cpus="1"', scenario)
        self.assertIn('zephyr_guest_type="virtualized"', scenario)
        self.assertIn("stress-dedicated|stress-rt)", INIT)

    def test_runner_allows_tcg_time_for_zephyr_sampling(self):
        self.assertIn('zephyr_timeout="${RT_ZEPHYR_TIMEOUT_SEC:-180}"', RUNNER)
        self.assertIn("expect ${zephyr_timeout} PERIODIC LATENCY COMPLETE", RUNNER)

    def test_all_formal_scenarios_budget_for_slow_tcg_guest_time(self):
        idle = RUNNER.split("idle)", 1)[1].split(";;", 1)[0]
        stress_noiso = RUNNER.split("stress-noiso)", 1)[1].split(";;", 1)[0]
        stress_rt = RUNNER.split("stress-rt)", 1)[1].split(";;", 1)[0]
        self.assertIn("runtime_scale=3", idle)
        self.assertIn("runtime_scale=3", stress_noiso)
        self.assertIn("runtime_scale=3", stress_rt)
        self.assertIn(
            "expected_wall_runtime_sec=$((expected_runtime_sec * runtime_scale))",
            RUNNER,
        )
        self.assertIn("experiment_timeout=$(( expected_wall_runtime_sec + 300 ))", RUNNER)

    def test_runner_prefers_per_scenario_calibration_over_fixed_default(self):
        self.assertIn("runtime-scales.env", RUNNER)
        self.assertIn("calibrated_scale", RUNNER)
        self.assertIn("runtime_scale_source", RUNNER)

    def test_runner_isolates_the_measurement_cpu(self):
        self.assertIn('rt_cpu="${RT_CPU:-1}"', RUNNER)
        self.assertIn('load_cpu=$((1 - rt_cpu))', RUNNER)
        self.assertRegex(RUNNER, r"isolcpus=\$\{rt_cpu\} nohz_full=\$\{rt_cpu\}")
        self.assertRegex(RUNNER, r"irqaffinity=\$\{load_cpu\}")
        self.assertRegex(RUNNER, r"rt_load_cpu=\$\{load_cpu\}")

    def test_guest_pins_stress_outside_the_measurement_cpu(self):
        self.assertIn('rt_load_cpu=*) load_cpu="${arg#rt_load_cpu=}"', INIT)
        self.assertIn('/bin/busybox taskset -c "$load_cpu" /bin/stress-ng', INIT)
        self.assertNotRegex(INIT, re.compile(r"stress-ng\s+--taskset"))

    def test_guest_moves_cyclictest_before_libnuma_starts(self):
        self.assertIn('all_cpus="0-$((cpu_total - 1))"', INIT)
        self.assertIn(
            '/bin/busybox taskset -c "$all_cpus" /bin/cyclictest -a "$cpu"',
            INIT,
        )

    def test_formal_runs_can_use_fixed_duration_instead_of_assumed_loop_rate(self):
        self.assertIn('duration_sec="${RT_DURATION_SEC:-0}"', RUNNER)
        self.assertIn("cyclictest_loops=0", RUNNER)
        self.assertIn("run_mode=duration", RUNNER)
        self.assertIn('rt_duration_sec=${duration_sec}', RUNNER)
        self.assertIn('-D "${duration_sec}s"', INIT)

    def test_duration_acceptance_uses_guest_uptime_not_runner_wall_time(self):
        self.assertIn(
            'echo "RT_CYCLICTEST_TIMING_START uptime_s=$start_uptime_s"',
            INIT,
        )
        self.assertIn(
            'echo "RT_CYCLICTEST_TIMING_END uptime_s=$end_uptime_s"',
            INIT,
        )
        self.assertIn(
            'guest_elapsed_s = end_uptime_s - start_uptime_s',
            RUNNER,
        )
        self.assertIn('minimum_guest_elapsed_s = Decimal(duration_sec) * Decimal("0.9")', RUNNER)
        self.assertNotIn('summary["total_samples"] * interval_us', RUNNER)
        self.assertNotIn("minimum_elapsed_ms = expected_runtime_sec * 900", RUNNER)

    def test_formal_runner_uses_progress_markers_and_a_no_progress_watchdog(self):
        self.assertIn('echo "RT_PROGRESS uptime_s=$progress_uptime_s"', INIT)
        self.assertIn("--progress-regex", RUNNER)
        self.assertIn("--progress-timeout", RUNNER)
        self.assertIn("RT_PROGRESS uptime_s=", RUNNER)

    def test_progress_watchdog_preserves_post_stall_forensics(self):
        self.assertIn('--qmp-sock "$qmp_sock"', RUNNER)
        self.assertIn('--forensics-dir "$out_dir/post-stall"', RUNNER)
        self.assertIn('post-stall/query-status.json', RUNNER)
        self.assertIn('post-stall/info-registers-2.json', RUNNER)

    def test_serial_log_records_host_timestamps(self):
        self.assertIn("--timestamp-lines", RUNNER)
        self.assertIn("host_monotonic_s=", RUNNER)

    def test_formal_metadata_records_that_realtime_trace_is_disabled(self):
        self.assertIn('linux_trace="${RT_LINUX_TRACE:-disabled}"', RUNNER)
        self.assertIn('printf \'realtime_trace=%s\\n\' "$linux_trace"', RUNNER)

    def test_guest_trace_mode_captures_timer_and_scheduler_events(self):
        self.assertIn('rt_trace=*) trace_mode="${arg#rt_trace=}"', INIT)
        self.assertIn('mount -t tracefs tracefs "$trace_dir"', INIT)
        self.assertIn("timer/hrtimer_expire_entry", INIT)
        self.assertIn("sched/sched_wakeup", INIT)
        self.assertIn("sched/sched_switch", INIT)
        self.assertIn("RT_FTRACE_DUMP_BEGIN", INIT)
        self.assertIn("RT_FTRACE_DUMP_READY encoding=gzip-base64", INIT)
        self.assertIn("/bin/busybox gzip -c /tmp/rt-ftrace.log", INIT)
        self.assertIn("/bin/busybox base64", INIT)
        self.assertIn('rt_trace=${linux_trace}', RUNNER)
        self.assertIn('linux-ftrace.txt', RUNNER)

    def test_guest_timerlat_mode_captures_irq_and_thread_latency(self):
        self.assertIn("disabled|events|timerlat", INIT)
        self.assertIn('echo timerlat > "$trace_dir/current_tracer"', INIT)
        self.assertIn('osnoise/timerlat_period_us', INIT)
        self.assertIn('RT_FTRACE_START mode=timerlat', INIT)
        self.assertIn('linux-timerlat.txt', RUNNER)
        self.assertIn('linux-timerlat-latency.py', RUNNER)

    def test_default_histogram_bound_keeps_formal_samples_in_range(self):
        self.assertIn('maxlat_us="${RT_MAXLAT_US:-20000}"', RUNNER)

    def test_deadline_tolerance_is_explicit_and_archived(self):
        self.assertIn(
            'deadline_tolerance_ns="${RT_DEADLINE_TOLERANCE_NS:-1000000}"',
            RUNNER,
        )
        self.assertIn('--tolerance-ns "$deadline_tolerance_ns"', RUNNER)
        self.assertIn("deadline_tolerance_ns=%s", RUNNER)

    def test_runner_can_build_an_official_baseline_worktree(self):
        self.assertIn('source_root="${RT_SOURCE_ROOT:-$repo_root}"', RUNNER)
        self.assertIn('cd "$source_root"', RUNNER)
        self.assertIn('find "$source_root/target"', RUNNER)
        self.assertIn('git -C "$source_root" rev-parse HEAD', RUNNER)

    def test_benchmark_burner_is_explicit_and_required_when_enabled(self):
        self.assertIn('burner_config="${RT_BURNER:-}"', RUNNER)
        self.assertIn('host_bootargs+=("rt_burner=${burner_config}")', RUNNER)
        self.assertIn('required.append(f"RT_BURNER_READY cpu=', RUNNER)
        self.assertIn("rt_burner=%s", RUNNER)

    def test_vmexit_diagnostics_can_be_disabled_for_upstream_dev(self):
        self.assertIn('vmexit_diagnostics="${RT_VMEXIT_DIAGNOSTICS:-1}"', RUNNER)
        self.assertIn("if vmexit_diagnostics:", RUNNER)
        self.assertIn('printf \'diagnostics=disabled\\n\'', RUNNER)

    def test_runner_accepts_a_baseline_specific_zephyr_template(self):
        self.assertIn('zephyr_template_override="${RT_ZEPHYR_TEMPLATE:-}"', RUNNER)
        self.assertIn('if [[ -n "$zephyr_template_override" ]]; then', RUNNER)
        self.assertIn('zephyr_template="$zephyr_template_override"', RUNNER)

    def test_runner_can_reuse_a_local_rootfs_to_avoid_baseline_downloads(self):
        self.assertIn('rootfs_override="${RT_ROOTFS:-}"', RUNNER)
        self.assertIn('rootfs_args=(--rootfs "$rootfs_override")', RUNNER)
        self.assertIn('"${rootfs_args[@]}"', RUNNER)

    def test_upstream_console_can_use_vm_stop_as_the_final_guest_marker(self):
        self.assertIn('require_init_done="${RT_REQUIRE_INIT_DONE:-1}"', RUNNER)
        self.assertIn("if require_init_done:", RUNNER)
        self.assertIn('init_done_step="expect ${result_drain_timeout} RT_INIT_DONE', RUNNER)
        self.assertIn("require_init_done=%s", RUNNER)
        self.assertIn("VM\\[1\\] PSCI_SYSTEM_OFF", RUNNER_STEPS)

    def test_outer_timeout_covers_boot_and_all_script_phases(self):
        self.assertIn("minimum_outer_timeout=$((", RUNNER)
        self.assertIn("timeout_sec >= minimum_outer_timeout", RUNNER)

    def test_zephyr_sampling_starts_inside_the_linux_workload_window(self):
        linux_start = RUNNER_STEPS.index(
            "expect ${linux_start_timeout} RT_CYCLICTEST_START"
        )
        zephyr_attach = RUNNER_STEPS.index(r"expect 10 Attached VM\[2\] console")
        zephyr_measurement = RUNNER_STEPS.index("${zephyr_measurement_steps}")
        linux_complete = RUNNER_STEPS.index(
            "expect ${experiment_timeout} RT_CYCLICTEST_COMPLETE"
        )
        self.assertLess(linux_start, zephyr_measurement)
        self.assertLess(zephyr_attach, zephyr_measurement)
        self.assertLess(zephyr_measurement, linux_complete)
        self.assertRegex(
            RUNNER,
            r"send-until 60 0\.5 g PERIODIC LATENCY START\n"
            r"(?:.*\n)*?expect \$\{zephyr_timeout\} "
            r"PERIODIC LATENCY COMPLETE samples=\$\{zephyr_samples\}",
        )
        self.assertIn('zephyr_start_gated="$(sed -n', RUNNER)
        self.assertIn('[[ "$zephyr_start_gated" == "1" ]]', RUNNER)
        self.assertIn("send-until 60 0.5 g PERIODIC LATENCY START", RUNNER)

    def test_runner_confirms_console_attachment_before_guest_input(self):
        zephyr_command = RUNNER_STEPS.index("cmd vm console 2")
        zephyr_attached = RUNNER_STEPS.index(r"expect 10 Attached VM\[2\] console")
        zephyr_measurement = RUNNER_STEPS.index("${zephyr_measurement_steps}")
        linux_command = RUNNER_STEPS.index("cmd vm console 1")
        linux_attached = RUNNER_STEPS.index(r"expect 10 Attached VM\[1\] console")
        self.assertLess(zephyr_command, zephyr_attached)
        self.assertLess(zephyr_attached, zephyr_measurement)
        self.assertLess(linux_command, linux_attached)

    def test_vmexit_snapshots_bound_the_zephyr_sampling_window(self):
        self.assertIn('expected at least three vmexit snapshots', RUNNER)
        self.assertIn('vmexit-zephyr-after.txt', RUNNER)
        zephyr_complete = RUNNER.index(
            "expect ${zephyr_timeout} PERIODIC LATENCY COMPLETE samples=${zephyr_samples}"
        )
        linux_attach = RUNNER.index("cmd vm console 1", zephyr_complete)
        linux_complete = RUNNER.index(
            "expect ${experiment_timeout} RT_CYCLICTEST_COMPLETE", linux_attach
        )
        middle_snapshot = RUNNER.index("${vmexit_after_zephyr_steps}", zephyr_complete)
        self.assertLess(zephyr_complete, linux_attach)
        self.assertLess(zephyr_complete, middle_snapshot)
        self.assertLess(middle_snapshot, linux_attach)
        self.assertLess(linux_attach, linux_complete)

    def test_linux_completion_is_drained_before_post_zephyr_diagnostics(self):
        linux_attach = RUNNER_STEPS.index("cmd vm console 1")
        linux_complete = RUNNER_STEPS.index(
            "expect ${experiment_timeout} RT_CYCLICTEST_COMPLETE"
        )
        linux_detach = RUNNER_STEPS.index("detach-if-attached", linux_complete)
        middle_snapshot = RUNNER_STEPS.index("${vmexit_after_zephyr_steps}")
        self.assertLess(linux_attach, linux_complete)
        self.assertLess(linux_complete, linux_detach)
        self.assertLess(middle_snapshot, linux_attach)

    def test_linux_console_is_explicitly_selected_after_zephyr(self):
        zephyr_complete = RUNNER.index(
            "expect ${zephyr_timeout} PERIODIC LATENCY COMPLETE samples=${zephyr_samples}"
        )
        linux_command = RUNNER_STEPS.index("cmd vm console 1")
        linux_attached = RUNNER_STEPS.index(
            r"expect 10 Attached VM\[1\] console", linux_command
        )
        self.assertLess(zephyr_complete, RUNNER.index("cmd vm console 1", zephyr_complete))
        self.assertLess(linux_command, linux_attached)
        self.assertIn("detach-if-attached", RUNNER_STEPS)

    def test_shared_runner_holds_linux_until_console_drain_finishes(self):
        self.assertIn("RT_HOLD_AFTER_COMPLETE=1", GUEST_SHARED_RUNNER)
        self.assertIn("RT_CYCLICTEST_HOLD_READY", RUNNER)
        self.assertIn("RT_CYCLICTEST_RELEASED", RUNNER)

    def test_dedicated_scenarios_require_zero_host_ticks_on_pcpu1(self):
        self.assertIn("host-periodic-ticks.csv", RUNNER)
        self.assertIn('host_tick_args+=(--require-zero-cpu "$dedicated_cpu")', RUNNER)
        self.assertIn('dedicated_cpus="1"', RUNNER)


if __name__ == "__main__":
    unittest.main()
