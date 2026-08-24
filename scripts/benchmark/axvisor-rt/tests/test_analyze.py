from __future__ import annotations

import importlib.util
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path


BENCHMARK_DIR = Path(__file__).resolve().parents[1]
ARTIFACT_NAMES = (
    "input_rootfs",
    "injected_rootfs_pre_run",
    "probe",
    "guest_runner",
    "raw_log",
    "axvisor_config",
    "vm_config",
    "qemu_config",
)
SPEC = importlib.util.spec_from_file_location("axvisor_rt_analyze", BENCHMARK_DIR / "analyze.py")
assert SPEC is not None and SPEC.loader is not None
analyze = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = analyze
SPEC.loader.exec_module(analyze)


def sample_line(metric: str, iteration: int, latency_ns: int, cpu: int = 0) -> str:
    target_ns = 1_000_000 + iteration * 10_000
    return (
        "AXVISOR_RT_SAMPLE schema=1 "
        f"metric={metric} iteration={iteration} cpu={cpu} "
        f"target_ns={target_ns} observed_ns={target_ns + latency_ns} "
        f"latency_ns={latency_ns}"
    )


def complete_capture_lines(workload: str = "idle") -> list[str]:
    lines = [
        "AXVISOR_RT_RUN_START",
        "AXVISOR_RT_GUEST_CPUS schema=1 online=2",
    ]
    if workload == "idle":
        lines.append("AXVISOR_RT_WORKLOAD_ACTIVE schema=1 kind=idle")
    elif workload == "cpu-stress":
        lines.append(
            "AXVISOR_RT_WORKLOAD_READY schema=1 kind=cpu-stress "
            "pid=42 cpu=1"
        )
        lines.append(
            "AXVISOR_RT_WORKLOAD_ACTIVE schema=1 kind=cpu-stress "
            "pid=42 cpu=1 affinity=1"
        )
    else:
        lines.append(
            "AXVISOR_RT_WORKLOAD_EXTERNAL schema=1 "
            f"verification=caller label={workload}"
        )
    lines.extend(
        [
            "AXVISOR_RT_CPUSTAT schema=1 phase=start cpu=0 "
            "user=10 nice=0 system=5 idle=80 iowait=1 irq=1 softirq=2 steal=0",
            "AXVISOR_RT_CPUSTAT schema=1 phase=start cpu=1 "
            "user=20 nice=0 system=4 idle=70 iowait=1 irq=0 softirq=1 steal=0",
        ]
    )
    lines.extend(
        sample_line(metric, 0, 5) for metric in analyze.EXPECTED_METRICS
    )
    lines.extend(
        [
            "AXVISOR_RT_CPUSTAT schema=1 phase=end cpu=0 "
            "user=30 nice=0 system=15 idle=140 iowait=1 irq=2 softirq=4 steal=0",
            "AXVISOR_RT_CPUSTAT schema=1 phase=end cpu=1 "
            "user=90 nice=0 system=14 idle=90 iowait=1 irq=1 softirq=4 steal=0",
        ]
    )
    if workload == "cpu-stress":
        lines.append(
            "AXVISOR_RT_WORKLOAD_STOPPED schema=1 kind=cpu-stress "
            "pid=42 cpu=1"
        )
        lines.append(
            "AXVISOR_RT_WORKLOAD_CLEANED schema=1 "
            "kind=cpu-stress pid=42 status=0"
        )
    lines.append("AXVISOR_RT_RUN_COMPLETE")
    return lines


def capture_metadata(raw_bytes: bytes, workload: str = "idle") -> dict[str, object]:
    artifacts = {
        name: {"path": f"/capture/{name}", "sha256": "a" * 64}
        for name in ARTIFACT_NAMES
    }
    artifacts["raw_log"]["sha256"] = hashlib.sha256(raw_bytes).hexdigest()
    return {
        "schema_version": 1,
        "run_id": "20260731T000000Z",
        "status": "capture_complete",
        "started_at": "2026-07-31T00:00:00Z",
        "finished_at": "2026-07-31T00:01:00Z",
        "repository": {
            "commit": "1" * 40,
            "dirty": True,
            "source_snapshot_sha256": "2" * 64,
            "tracked_diff_sha256": "3" * 64,
            "untracked_source_file_count": 1,
            "untracked_source_manifest_sha256": "4" * 64,
        },
        "host": {
            "system": "Linux",
            "release": "test",
            "machine": "x86_64",
        },
        "qemu": {
            "binary": "qemu-system-aarch64",
            "version": "QEMU emulator version test",
            "acceleration": "tcg",
            "machine": "virt,virtualization=on,gic-version=3",
            "cpu": "cortex-a72",
            "host_cpu_count": 4,
            "exit_code": 0,
        },
        "guest": {
            "architecture": "aarch64",
            "vcpu_count": 2,
            "profile": "partitioned",
            "dedicated_host_cpu_ids": [2, 3],
            "vm_config": (
                "os/axvisor/configs/vms/qemu/aarch64/"
                "linux-smp2-dedicated.toml"
            ),
        },
        "benchmark": {
            "iterations": 1,
            "warmup_iterations": 0,
            "period_ns": 1_000_000,
            "guest_cpu": 0,
            "fifo_priority": 80,
            "workload": workload,
            "metrics": [
                "periodic_jitter",
                "dispatch_latency",
                "emulated_irq_response",
            ],
        },
        "artifacts": artifacts,
    }


def write_capture(
    output: Path,
    lines: list[str],
    *,
    workload: str = "idle",
) -> tuple[Path, Path]:
    raw_log = output / "raw-console.log"
    raw_bytes = ("\n".join(lines) + "\n").encode()
    raw_log.write_bytes(raw_bytes)
    metadata = output / "metadata.json"
    metadata.write_text(
        json.dumps(capture_metadata(raw_bytes, workload)), encoding="utf-8"
    )
    return raw_log, metadata


class AnalyzerTests(unittest.TestCase):
    def test_summarizes_all_metrics_with_nearest_rank_percentiles(self) -> None:
        lines = ["unrelated boot log"]
        for metric in analyze.EXPECTED_METRICS:
            lines.extend(sample_line(metric, index, value) for index, value in enumerate([1, 2, 3, 4]))

        summary = analyze.summarize_samples(analyze.parse_samples(lines))

        for metric in analyze.EXPECTED_METRICS:
            metric_summary = summary["metrics"][metric]
            self.assertEqual(metric_summary["count"], 4)
            self.assertEqual(metric_summary["min_ns"], 1)
            self.assertEqual(metric_summary["max_ns"], 4)
            self.assertEqual(metric_summary["mean_ns"], 2.5)
            self.assertEqual(metric_summary["p50_ns"], 2)
            self.assertEqual(metric_summary["p90_ns"], 4)
            self.assertEqual(metric_summary["p99_ns"], 4)
            self.assertEqual(metric_summary["p999_ns"], 4)

    def test_summary_is_independent_of_sample_order(self) -> None:
        lines = [
            sample_line(metric, iteration, latency)
            for metric in analyze.EXPECTED_METRICS
            for iteration, latency in enumerate([11, 7, 19])
        ]

        forward = analyze.summarize_samples(analyze.parse_samples(lines))
        reverse = analyze.summarize_samples(analyze.parse_samples(reversed(lines)))

        self.assertEqual(forward, reverse)

    def test_rejects_inconsistent_latency(self) -> None:
        line = (
            "AXVISOR_RT_SAMPLE schema=1 metric=periodic_jitter iteration=0 cpu=0 "
            "target_ns=100 observed_ns=150 latency_ns=49"
        )

        with self.assertRaisesRegex(analyze.AnalysisError, "does not equal"):
            analyze.parse_samples([line])

    def test_rejects_duplicate_sample_identity(self) -> None:
        line = sample_line("periodic_jitter", 0, 5)

        with self.assertRaisesRegex(analyze.AnalysisError, "duplicate"):
            analyze.parse_samples([line, line])

    def test_requires_each_metric(self) -> None:
        samples = analyze.parse_samples([sample_line("periodic_jitter", 0, 5)])

        with self.assertRaisesRegex(analyze.AnalysisError, "missing required metrics"):
            analyze.summarize_samples(samples)

    def test_rejects_capture_shorter_than_metadata_iteration_contract(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            raw_log, metadata = write_capture(output, complete_capture_lines())
            metadata_value = json.loads(metadata.read_text(encoding="utf-8"))
            metadata_value["benchmark"]["iterations"] = 2
            metadata.write_text(json.dumps(metadata_value), encoding="utf-8")

            with self.assertRaisesRegex(analyze.AnalysisError, "expected 2 samples"):
                analyze.analyze_file(raw_log, metadata)

    def test_accepts_complete_two_cpu_idle_capture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            raw_log, metadata = write_capture(
                Path(directory), complete_capture_lines()
            )

            summary = analyze.analyze_file(raw_log, metadata)

            self.assertEqual(summary["metadata"]["guest"]["vcpu_count"], 2)
            self.assertEqual(summary["cpu_load"]["cpus"]["0"]["total_ticks"], 93)
            self.assertEqual(summary["cpu_load"]["cpus"]["1"]["busy_ticks"], 84)

    def test_rejects_missing_cpu_load_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            lines = [
                line
                for line in complete_capture_lines()
                if not line.startswith("AXVISOR_RT_CPUSTAT ")
            ]
            raw_log, metadata = write_capture(Path(directory), lines)

            with self.assertRaisesRegex(analyze.AnalysisError, "CPUSTAT"):
                analyze.analyze_file(raw_log, metadata)

    def test_rejects_regressing_cpu_counter(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            lines = complete_capture_lines()
            end_index = next(
                index
                for index, line in enumerate(lines)
                if line.startswith("AXVISOR_RT_CPUSTAT schema=1 phase=end cpu=0 ")
            )
            lines[end_index] = (
                "AXVISOR_RT_CPUSTAT schema=1 phase=end cpu=0 "
                "user=9 nice=0 system=15 idle=140 iowait=1 irq=2 softirq=4 steal=0"
            )
            raw_log, metadata = write_capture(Path(directory), lines)

            with self.assertRaisesRegex(analyze.AnalysisError, "regressed"):
                analyze.analyze_file(raw_log, metadata)

    def test_rejects_missing_or_failed_run_completion(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            for suffix, expected in (
                (["AXVISOR_RT_RUN_COMPLETE"], "RUN_COMPLETE"),
                (["AXVISOR_RT_RUN_FAILED status=1"], "RUN_FAILED"),
            ):
                with self.subTest(expected=expected):
                    lines = complete_capture_lines()
                    lines = [line for line in lines if line not in suffix]
                    if expected == "RUN_FAILED":
                        lines.insert(-1, suffix[0])
                    raw_log, metadata = write_capture(output, lines)
                    with self.assertRaisesRegex(analyze.AnalysisError, expected):
                        analyze.analyze_file(raw_log, metadata)

    def test_rejects_guest_cpu_count_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            lines = complete_capture_lines()
            lines[1] = "AXVISOR_RT_GUEST_CPUS schema=1 online=1"
            raw_log, metadata = write_capture(Path(directory), lines)

            with self.assertRaisesRegex(analyze.AnalysisError, "online CPU"):
                analyze.analyze_file(raw_log, metadata)

    def test_rejects_workload_evidence_that_does_not_match_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            raw_log, metadata = write_capture(
                Path(directory), complete_capture_lines(), workload="cpu-stress"
            )

            with self.assertRaisesRegex(analyze.AnalysisError, "READY"):
                analyze.analyze_file(raw_log, metadata)

    def test_rejects_incomplete_cpu_stress_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            lines = complete_capture_lines("cpu-stress")
            lines = [
                line
                for line in lines
                if not line.startswith("AXVISOR_RT_WORKLOAD_CLEANED ")
            ]
            raw_log, metadata = write_capture(
                Path(directory), lines, workload="cpu-stress"
            )

            with self.assertRaisesRegex(analyze.AnalysisError, "CLEANED"):
                analyze.analyze_file(raw_log, metadata)

    def test_rejects_cpu_stress_without_exact_probe_lifecycle(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            base_lines = complete_capture_lines("cpu-stress")
            for mutation, expected in (
                (
                    [
                        line
                        for line in base_lines
                        if not line.startswith("AXVISOR_RT_WORKLOAD_READY ")
                    ],
                    "READY",
                ),
                (
                    [line.replace("pid=42 cpu=1", "pid=7 cpu=1")
                     if line.startswith("AXVISOR_RT_WORKLOAD_READY ") else line
                     for line in base_lines],
                    "PID",
                ),
            ):
                with self.subTest(expected=expected):
                    raw_log, metadata = write_capture(
                        output, mutation, workload="cpu-stress"
                    )
                    with self.assertRaisesRegex(analyze.AnalysisError, expected):
                        analyze.analyze_file(raw_log, metadata)

    def test_rejects_cpu_stress_that_stops_before_measurement_end(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            lines = complete_capture_lines("cpu-stress")
            stopped = next(
                line
                for line in lines
                if line.startswith("AXVISOR_RT_WORKLOAD_STOPPED ")
            )
            lines.remove(stopped)
            start_index = next(
                index
                for index, line in enumerate(lines)
                if line.startswith("AXVISOR_RT_CPUSTAT schema=1 phase=start cpu=0 ")
            )
            lines.insert(start_index, stopped)
            raw_log, metadata = write_capture(
                Path(directory), lines, workload="cpu-stress"
            )

            with self.assertRaisesRegex(analyze.AnalysisError, "stopped before"):
                analyze.analyze_file(raw_log, metadata)

    def test_rejects_cpu_stress_below_minimum_busy_load(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            lines = complete_capture_lines("cpu-stress")
            end_index = next(
                index
                for index, line in enumerate(lines)
                if line.startswith("AXVISOR_RT_CPUSTAT schema=1 phase=end cpu=1 ")
            )
            lines[end_index] = (
                "AXVISOR_RT_CPUSTAT schema=1 phase=end cpu=1 "
                "user=20 nice=0 system=4 idle=170 iowait=1 irq=0 softirq=1 steal=0"
            )
            raw_log, metadata = write_capture(
                Path(directory), lines, workload="cpu-stress"
            )

            with self.assertRaisesRegex(analyze.AnalysisError, "at least 50"):
                analyze.analyze_file(raw_log, metadata)

    def test_rejects_minimal_unknown_or_incomplete_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            raw_log, metadata = write_capture(output, complete_capture_lines())
            complete = json.loads(metadata.read_text(encoding="utf-8"))
            mutations = []

            missing_repository = json.loads(json.dumps(complete))
            del missing_repository["repository"]
            mutations.append((missing_repository, "repository"))

            unknown_top_level = json.loads(json.dumps(complete))
            unknown_top_level["unvalidated"] = True
            mutations.append((unknown_top_level, "unknown fields"))

            unknown_snapshot = json.loads(json.dumps(complete))
            unknown_snapshot["repository"]["source_snapshot_sha256"] = "unknown"
            mutations.append((unknown_snapshot, "source_snapshot_sha256"))

            missing_probe = json.loads(json.dumps(complete))
            del missing_probe["artifacts"]["probe"]
            mutations.append((missing_probe, "probe"))

            for value, expected in mutations:
                with self.subTest(expected=expected):
                    metadata.write_text(json.dumps(value), encoding="utf-8")
                    with self.assertRaisesRegex(analyze.AnalysisError, expected):
                        analyze.analyze_file(raw_log, metadata)

    def test_rejects_interrupted_capture_running_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            raw_log, metadata = write_capture(
                Path(directory), complete_capture_lines()
            )
            value = json.loads(metadata.read_text(encoding="utf-8"))
            value["status"] = "capture_running"
            value["finished_at"] = None
            value["qemu"]["exit_code"] = None
            del value["artifacts"]["raw_log"]
            metadata.write_text(json.dumps(value), encoding="utf-8")

            with self.assertRaisesRegex(analyze.AnalysisError, "capture_complete"):
                analyze.analyze_file(raw_log, metadata)

    def test_rejects_unsuccessful_metadata_or_raw_log_hash_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            raw_log, metadata = write_capture(output, complete_capture_lines())
            metadata_value = json.loads(metadata.read_text(encoding="utf-8"))

            for mutation, expected in (
                (("status", "failed"), "capture_complete"),
                (("exit_code", 1), "exit code"),
                (("sha256", "0" * 64), "SHA-256"),
            ):
                with self.subTest(expected=expected):
                    value = json.loads(json.dumps(metadata_value))
                    key, replacement = mutation
                    if key == "status":
                        value["status"] = replacement
                    elif key == "exit_code":
                        value["qemu"]["exit_code"] = replacement
                    else:
                        value["artifacts"]["raw_log"]["sha256"] = replacement
                    metadata.write_text(json.dumps(value), encoding="utf-8")
                    with self.assertRaisesRegex(analyze.AnalysisError, expected):
                        analyze.analyze_file(raw_log, metadata)

    def test_metadata_example_is_explicitly_unmeasured(self) -> None:
        example = json.loads(
            (BENCHMARK_DIR / "metadata.example.json").read_text(encoding="utf-8")
        )
        schema = json.loads(
            (BENCHMARK_DIR / "metadata.schema.json").read_text(encoding="utf-8")
        )

        self.assertEqual(example["status"], "planned")
        self.assertEqual(example["artifacts"], {})
        self.assertRegex(
            example["benchmark"]["workload"],
            schema["properties"]["benchmark"]["properties"]["workload"]["pattern"],
        )
        self.assertNotIn("metrics", example)

        repository = schema["properties"]["repository"]
        self.assertEqual(
            repository["properties"]["source_snapshot_sha256"]["pattern"],
            "^[0-9a-f]{64}$",
        )
        completed_rule = next(
            rule
            for rule in schema["allOf"]
            if rule["if"]["properties"]["status"].get("const")
            == "capture_complete"
        )
        self.assertEqual(
            set(completed_rule["then"]["properties"]["artifacts"]["required"]),
            set(ARTIFACT_NAMES),
        )
        self.assertIn("capture_running", schema["properties"]["status"]["enum"])


if __name__ == "__main__":
    unittest.main()
