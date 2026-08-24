from __future__ import annotations

import gzip
import hashlib
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


BENCHMARK_DIR = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "axvisor_rt_analyze_starry_board",
    BENCHMARK_DIR / "analyze_starry_board.py",
)
assert SPEC is not None and SPEC.loader is not None
starry = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = starry
SPEC.loader.exec_module(starry)


def sample_line(metric: str, iteration: int, latency_ns: int, cpu: int = 0) -> str:
    target_ns = 1_000_000 + iteration * 10_000
    return (
        "AXVISOR_RT_SAMPLE schema=1 "
        f"metric={metric} iteration={iteration} cpu={cpu} "
        f"target_ns={target_ns} observed_ns={target_ns + latency_ns} "
        f"latency_ns={latency_ns}"
    )


def capture_lines(*, iterations: int = 2, workload: str = "idle") -> list[str]:
    lines = [
        "AXVISOR_RT_RUN_START",
        "AXVISOR_RT_GUEST_CPUS schema=1 os=starryos online=2",
        "AXVISOR_RT_STARRY_CAPTURE schema=1 "
        f"iterations={iterations} warmup=1 period_us=1000 measurement_cpu=0 "
        f"stress_cpu=1 fifo_priority=80 workload={workload}",
    ]
    if workload == "idle":
        lines.append("AXVISOR_RT_WORKLOAD_ACTIVE schema=1 kind=idle")
    else:
        lines.extend(
            [
                "AXVISOR_RT_WORKLOAD_READY schema=1 kind=cpu-stress pid=42 cpu=1",
                "AXVISOR_RT_WORKLOAD_ACTIVE schema=1 kind=cpu-stress "
                "pid=42 cpu=1 affinity=1",
            ]
        )

    for metric in starry.EXPECTED_METRICS:
        lines.extend(
            sample_line(metric, iteration, 10 + iteration)
            for iteration in range(iterations)
        )
        lines.append(
            "AXVISOR_RT_METRIC_COMPLETE schema=1 "
            f"metric={metric} count={iterations}"
        )

    if workload == "cpu-stress":
        lines.extend(
            [
                "AXVISOR_RT_WORKLOAD_STOPPED schema=1 kind=cpu-stress "
                "pid=42 cpu=1",
                "AXVISOR_RT_WORKLOAD_CLEANED schema=1 kind=cpu-stress "
                "pid=42 status=0",
            ]
        )
    lines.append("AXVISOR_RT_RUN_COMPLETE")
    return lines


def write_raw(directory: Path, lines: list[str]) -> Path:
    raw = directory / "raw.log"
    raw.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return raw


def write_direct_irq_traces(
    directory: Path, host_noise_pcpu: int | None = None
) -> tuple[Path, Path, int, str]:
    host = directory / "host.log"
    host_noise_lines: list[str] = []
    if host_noise_pcpu is not None:
        mask = 1 << host_noise_pcpu
        host_noise_lines = [
            "AXVISOR_RT_HOST_NOISE schema=1 "
            f"requested_pcpu={host_noise_pcpu} affinity_mask={mask:#x} "
            f"observed_pcpu_mask={mask:#x} max_duration_ms=180000 "
            "start_ticks=50 end_ticks=1050 elapsed_ticks=1000 elapsed_ns=41666 "
            "loop_iterations=400 stop_reason=guest-complete intensity=busy-loop",
            "AXVISOR_RT_HOST_NOISE_PCPU schema=1 "
            f"pcpu={host_noise_pcpu} observed_wall_ticks=1000",
        ]
    host.write_text(
        "\n".join(
            [
                "AXVISOR_RT_HOST_TRACE schema=1 vm=1 counter_frequency_hz=24000000 start_ticks=100 end_ticks=1000 records=2 dropped=0 incomplete=0 failed_injections=0 unowned_virtual_timer_irqs=0 counter_frequency_mismatches=0",
                *host_noise_lines,
                "AXVISOR_RT_HOST_IRQ schema=1 sequence=0 vm=1 vcpu=0 pcpu=1 physical_irq=27 virtual_irq=27 host_counter_ticks=510 guest_counter_ticks=210 forwarding_ticks=3 injected=1",
                "AXVISOR_RT_HOST_IRQ schema=1 sequence=1 vm=1 vcpu=1 pcpu=2 physical_irq=27 virtual_irq=27 host_counter_ticks=620 guest_counter_ticks=220 forwarding_ticks=4 injected=1",
                "AXVISOR_RT_HOST_PCPU schema=1 pcpu=1 wall_ticks=900 running_ticks=600 idle_ticks=300",
                "AXVISOR_RT_HOST_PCPU schema=1 pcpu=2 wall_ticks=900 running_ticks=500 idle_ticks=400",
                "AXVISOR_RT_HOST_VCPU schema=1 vm=1 vcpu=0 run_count=4 run_ticks=500 max_run_ticks=200 wait_count=2 wait_ticks=300 max_wait_ticks=180 pcpu_mask=0x2 migrations=0",
                "AXVISOR_RT_HOST_VCPU schema=1 vm=1 vcpu=1 run_count=3 run_ticks=450 max_run_ticks=210 wait_count=2 wait_ticks=320 max_wait_ticks=190 pcpu_mask=0x4 migrations=0",
                "AXVISOR_RT_HOST_TRACE_COMPLETE schema=1 records=2",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    guest = directory / "guest.log.gz"
    with gzip.open(guest, "wt", encoding="utf-8", newline="\n") as output:
        output.write(
            "\n".join(
                [
                    "AXVISOR_RT_GUEST_IRQ_TRACE schema=1 counter_frequency_hz=24000000 start_ticks=10 end_ticks=900 records=2 dropped=0 incomplete=0",
                    "AXVISOR_RT_GUEST_IRQ schema=1 sequence=0 vcpu=0 irq=27 guest_entry_ticks=216 handler_ticks=8",
                    "AXVISOR_RT_GUEST_IRQ schema=1 sequence=1 vcpu=1 irq=27 guest_entry_ticks=229 handler_ticks=7",
                    "AXVISOR_RT_GUEST_IRQ_TRACE_COMPLETE schema=1 records=2",
                ]
            )
            + "\n"
        )
    guest_bytes = guest.read_bytes()
    return host, guest, len(guest_bytes), hashlib.sha256(guest_bytes).hexdigest()


class StarryBoardAnalyzerTests(unittest.TestCase):
    def test_merges_direct_irq_and_independent_host_accounting(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            host, guest, guest_bytes, guest_sha256 = write_direct_irq_traces(root)
            lines = capture_lines()
            lines.insert(
                -1,
                "AXVISOR_RT_GUEST_IRQ_TRACE_FILE schema=1 "
                "path=/var/lib/axvisor-rt/guest-timer-trace.log.gz compression=gzip "
                f"bytes={guest_bytes} sha256={guest_sha256}",
            )
            raw = write_raw(root, lines)

            result = starry.analyze_starry_file(
                raw,
                profile="partitioned",
                host_trace_path=host,
                guest_irq_trace_path=guest,
            )

        direct = result["metrics"]["virtual_timer_injection_to_guest_irq"]
        self.assertEqual(direct["samples_ns"], [250, 375])
        self.assertEqual(result["host_pcpu_accounting"]["status"], "collected")
        self.assertEqual(len(result["host_pcpu_accounting"]["pcpus"]), 2)
        self.assertEqual(result["host_noise"]["status"], "not-configured")

    def test_requires_expected_host_noise_placement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            host, guest, guest_bytes, guest_sha256 = write_direct_irq_traces(
                root, host_noise_pcpu=3
            )
            lines = capture_lines()
            lines.insert(
                -1,
                "AXVISOR_RT_GUEST_IRQ_TRACE_FILE schema=1 "
                "path=/var/lib/axvisor-rt/guest-timer-trace.log.gz compression=gzip "
                f"bytes={guest_bytes} sha256={guest_sha256}",
            )
            raw = write_raw(root, lines)

            result = starry.analyze_starry_file(
                raw,
                profile="partitioned",
                host_trace_path=host,
                guest_irq_trace_path=guest,
                expected_host_noise_pcpu=3,
            )

        self.assertEqual(result["host_noise"]["status"], "collected")
        self.assertEqual(result["host_noise"]["requested_pcpu"], 3)
        self.assertTrue(result["host_noise"]["covers_host_trace"])

    def test_rejects_missing_expected_host_noise(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            host, guest, guest_bytes, guest_sha256 = write_direct_irq_traces(root)
            lines = capture_lines()
            lines.insert(
                -1,
                "AXVISOR_RT_GUEST_IRQ_TRACE_FILE schema=1 "
                "path=/var/lib/axvisor-rt/guest-timer-trace.log.gz compression=gzip "
                f"bytes={guest_bytes} sha256={guest_sha256}",
            )
            raw = write_raw(root, lines)

            with self.assertRaisesRegex(starry.AnalysisError, "missing required host-noise"):
                starry.analyze_starry_file(
                    raw,
                    profile="shared",
                    host_trace_path=host,
                    guest_irq_trace_path=guest,
                    expected_host_noise_pcpu=1,
                )

    def test_accepts_lossless_idle_capture_and_labels_timerfd_proxy(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            raw = write_raw(Path(directory), capture_lines())

            result = starry.analyze_starry_file(
                raw,
                profile="shared",
                expected_workload="idle",
                expected_iterations=2,
                filesystem_state="unclean-orphans-raw-stable-after-copy-repair",
            )

        self.assertEqual(result["capture"]["sample_count"], 6)
        self.assertEqual(result["capture"]["vcpu_count"], 2)
        self.assertEqual(result["capture"]["profile"], "shared")
        self.assertEqual(
            result["input"]["snapshot_filesystem_state"],
            "unclean-orphans-raw-stable-after-copy-repair",
        )
        self.assertEqual(result["metrics"]["periodic_jitter"]["count"], 2)
        self.assertEqual(
            result["metric_semantics"]["emulated_irq_response"]["authority"],
            "proxy",
        )
        self.assertEqual(result["host_pcpu_accounting"]["status"], "not-collected")

    def test_soak_capture_records_the_dedicated_vm_config(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            raw = write_raw(Path(directory), capture_lines())

            result = starry.analyze_starry_file(
                raw,
                profile="shared",
                soak=True,
            )

        self.assertEqual(
            result["profile_contract"]["vm_config"],
            "scripts/benchmark/axvisor-rt/config/"
            "starry-orangepi-5-plus-smp2-soak-shared.toml",
        )
        self.assertTrue(result["profile_contract"]["soak"])

    def test_rejects_missing_sample_even_when_completion_count_claims_success(self) -> None:
        lines = capture_lines()
        lines.remove(sample_line("dispatch_latency", 1, 11))
        with tempfile.TemporaryDirectory() as directory:
            raw = write_raw(Path(directory), lines)
            with self.assertRaisesRegex(starry.AnalysisError, "expected 2 samples"):
                starry.analyze_starry_file(raw, profile="partitioned")

    def test_rejects_sample_from_wrong_measurement_cpu(self) -> None:
        lines = capture_lines()
        index = lines.index(sample_line("periodic_jitter", 0, 10))
        lines[index] = sample_line("periodic_jitter", 0, 10, cpu=1)
        with tempfile.TemporaryDirectory() as directory:
            raw = write_raw(Path(directory), lines)
            with self.assertRaisesRegex(starry.AnalysisError, "measurement CPU 0"):
                starry.analyze_starry_file(raw, profile="shared")

    def test_rejects_workload_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            raw = write_raw(Path(directory), capture_lines(workload="cpu-stress"))
            with self.assertRaisesRegex(starry.AnalysisError, "expected workload idle"):
                starry.analyze_starry_file(
                    raw,
                    profile="partitioned",
                    expected_workload="idle",
                )

    def test_accepts_complete_cpu_stress_lifecycle(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            raw = write_raw(Path(directory), capture_lines(workload="cpu-stress"))
            result = starry.analyze_starry_file(
                raw,
                profile="partitioned",
                expected_workload="cpu-stress",
            )

        self.assertEqual(result["capture"]["workload"], "cpu-stress")

    def test_rejects_duplicate_metric_completion_marker(self) -> None:
        lines = capture_lines()
        lines.insert(
            -1,
            "AXVISOR_RT_METRIC_COMPLETE schema=1 "
            "metric=periodic_jitter count=2",
        )
        with tempfile.TemporaryDirectory() as directory:
            raw = write_raw(Path(directory), lines)
            with self.assertRaisesRegex(starry.AnalysisError, "exactly one completion"):
                starry.analyze_starry_file(raw, profile="shared")


if __name__ == "__main__":
    unittest.main()
