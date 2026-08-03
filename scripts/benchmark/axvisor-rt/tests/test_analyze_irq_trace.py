from __future__ import annotations

import gzip
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "analyze_irq_trace.py"


def load_module():
    spec = importlib.util.spec_from_file_location("analyze_irq_trace", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load direct IRQ trace analyzer")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


HOST_TRACE = """\
AXVISOR_RT_HOST_TRACE schema=1 vm=1 counter_frequency_hz=24000000 start_ticks=100 end_ticks=1000 records=3 dropped=0 failed_injections=0 unowned_virtual_timer_irqs=0
AXVISOR_RT_HOST_IRQ schema=1 sequence=0 vm=1 vcpu=0 pcpu=1 physical_irq=27 virtual_irq=27 host_counter_ticks=510 guest_counter_ticks=210 forwarding_ticks=3 injected=1
AXVISOR_RT_HOST_IRQ schema=1 sequence=1 vm=1 vcpu=1 pcpu=2 physical_irq=27 virtual_irq=27 host_counter_ticks=620 guest_counter_ticks=220 forwarding_ticks=4 injected=1
AXVISOR_RT_HOST_IRQ schema=1 sequence=2 vm=1 vcpu=0 pcpu=1 physical_irq=27 virtual_irq=27 host_counter_ticks=810 guest_counter_ticks=510 forwarding_ticks=5 injected=1
AXVISOR_RT_HOST_PCPU schema=1 pcpu=1 wall_ticks=900 running_ticks=600 idle_ticks=300
AXVISOR_RT_HOST_PCPU schema=1 pcpu=2 wall_ticks=900 running_ticks=500 idle_ticks=400
AXVISOR_RT_HOST_VCPU schema=1 vm=1 vcpu=0 run_count=4 run_ticks=500 max_run_ticks=200 wait_count=2 wait_ticks=300 max_wait_ticks=180 pcpu_mask=0x2 migrations=0
AXVISOR_RT_HOST_VCPU schema=1 vm=1 vcpu=1 run_count=3 run_ticks=450 max_run_ticks=210 wait_count=2 wait_ticks=320 max_wait_ticks=190 pcpu_mask=0x4 migrations=0
AXVISOR_RT_HOST_TRACE_COMPLETE schema=1 records=3
"""

GUEST_TRACE = """\
AXVISOR_RT_GUEST_IRQ_TRACE schema=1 counter_frequency_hz=24000000 start_ticks=10 end_ticks=900 records=3 dropped=0 incomplete=0
AXVISOR_RT_GUEST_IRQ schema=1 sequence=0 vcpu=0 irq=27 guest_entry_ticks=216 handler_ticks=8
AXVISOR_RT_GUEST_IRQ schema=1 sequence=1 vcpu=1 irq=27 guest_entry_ticks=229 handler_ticks=7
AXVISOR_RT_GUEST_IRQ schema=1 sequence=2 vcpu=0 irq=27 guest_entry_ticks=522 handler_ticks=9
AXVISOR_RT_GUEST_IRQ_TRACE_COMPLETE schema=1 records=3
"""

HOST_NOISE = """\
AXVISOR_RT_HOST_NOISE schema=1 requested_pcpu=1 affinity_mask=0x2 observed_pcpu_mask=0x2 max_duration_ms=180000 start_ticks=50 end_ticks=1050 elapsed_ticks=1000 elapsed_ns=41666 loop_iterations=400 stop_reason=guest-complete intensity=busy-loop
AXVISOR_RT_HOST_NOISE_PCPU schema=1 pcpu=1 observed_wall_ticks=1000
"""


class DirectIrqTraceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.module = load_module()

    def write_traces(self, host: str = HOST_TRACE, guest: str = GUEST_TRACE):
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        host_path = root / "host.log"
        guest_path = root / "guest.log.gz"
        host_path.write_text(host, encoding="utf-8")
        with gzip.open(guest_path, "wt", encoding="utf-8", newline="\n") as output:
            output.write(guest)
        return temporary, host_path, guest_path

    def test_pairs_in_guest_counter_domain_and_reports_latency(self) -> None:
        temporary, host_path, guest_path = self.write_traces()
        with temporary:
            result = self.module.analyze_irq_traces(host_path, guest_path)

        metric = result["virtual_timer_injection_to_guest_irq_ns"]
        self.assertEqual(metric["count"], 3)
        self.assertEqual(metric["samples_ns"], [250, 375, 500])
        self.assertEqual(metric["max_ns"], 500)
        self.assertEqual(result["counter_validation"]["frequency_hz"], 24_000_000)
        self.assertEqual(result["counter_validation"]["domain"], "guest-virtual-counter")
        self.assertEqual(len(result["host_accounting"]["pcpus"]), 2)
        self.assertEqual(len(result["host_accounting"]["vcpus"]), 2)
        self.assertIsNone(result["host_noise"])
        self.assertEqual(
            result["lossless"],
            {
                "guest": {
                    "counter_frequency_mismatches": 0,
                    "dropped": 0,
                    "failed_injections": 0,
                    "incomplete": 0,
                    "records": 3,
                    "unowned_virtual_timer_irqs": 0,
                },
                "host": {
                    "counter_frequency_mismatches": 0,
                    "dropped": 0,
                    "failed_injections": 0,
                    "incomplete": 0,
                    "records": 3,
                    "unowned_virtual_timer_irqs": 0,
                },
            },
        )

    def test_validates_host_noise_placement_and_trace_coverage(self) -> None:
        host = HOST_TRACE.replace(
            "AXVISOR_RT_HOST_TRACE schema=1",
            "AXVISOR_RT_HOST_TRACE schema=1",
        ).replace(
            "\nAXVISOR_RT_HOST_IRQ",
            "\n" + HOST_NOISE + "AXVISOR_RT_HOST_IRQ",
            1,
        )
        temporary, host_path, guest_path = self.write_traces(host=host)
        with temporary:
            result = self.module.analyze_irq_traces(host_path, guest_path)

        noise = result["host_noise"]
        self.assertEqual(noise["requested_pcpu"], 1)
        self.assertEqual(noise["observed_pcpu_mask"], 0x2)
        self.assertTrue(noise["covers_host_trace"])
        self.assertEqual(
            noise["pcpus"], [{"pcpu": 1, "observed_wall_ticks": 1000}]
        )

    def test_rejects_host_noise_that_escapes_requested_cpu(self) -> None:
        invalid_noise = HOST_NOISE.replace("observed_pcpu_mask=0x2", "observed_pcpu_mask=0xa")
        host = HOST_TRACE.replace(
            "\nAXVISOR_RT_HOST_IRQ",
            "\n" + invalid_noise + "AXVISOR_RT_HOST_IRQ",
            1,
        )
        temporary, host_path, guest_path = self.write_traces(host=host)
        with temporary, self.assertRaisesRegex(
            self.module.AnalysisError, "escaped its singleton pCPU"
        ):
            self.module.analyze_irq_traces(host_path, guest_path)

    def test_rejects_counter_frequency_mismatch(self) -> None:
        guest = GUEST_TRACE.replace("counter_frequency_hz=24000000", "counter_frequency_hz=25000000")
        temporary, host_path, guest_path = self.write_traces(guest=guest)
        with temporary, self.assertRaisesRegex(
            self.module.AnalysisError, "counter frequencies differ"
        ):
            self.module.analyze_irq_traces(host_path, guest_path)

    def test_rejects_trace_overflow(self) -> None:
        host = HOST_TRACE.replace("records=3 dropped=0", "records=3 dropped=1", 1)
        temporary, host_path, guest_path = self.write_traces(host=host)
        with temporary, self.assertRaisesRegex(self.module.AnalysisError, "dropped"):
            self.module.analyze_irq_traces(host_path, guest_path)

    def test_rejects_timer_ppi_observed_without_current_vcpu(self) -> None:
        host = HOST_TRACE.replace(
            "unowned_virtual_timer_irqs=0", "unowned_virtual_timer_irqs=1", 1
        )
        temporary, host_path, guest_path = self.write_traces(host=host)
        with temporary, self.assertRaisesRegex(
            self.module.AnalysisError, "unowned_virtual_timer_irqs"
        ):
            self.module.analyze_irq_traces(host_path, guest_path)

    def test_rejects_unpaired_event_after_alignment(self) -> None:
        host = HOST_TRACE.replace(
            "AXVISOR_RT_HOST_IRQ schema=1 sequence=2",
            "AXVISOR_RT_HOST_IRQ schema=1 sequence=9 vm=1 vcpu=0 pcpu=1 physical_irq=27 virtual_irq=27 host_counter_ticks=700 guest_counter_ticks=400 forwarding_ticks=2 injected=1\nAXVISOR_RT_HOST_IRQ schema=1 sequence=2",
        ).replace("records=3", "records=4")
        temporary, host_path, guest_path = self.write_traces(host=host)
        with temporary, self.assertRaisesRegex(
            self.module.AnalysisError, "unpaired host injection"
        ):
            self.module.analyze_irq_traces(host_path, guest_path)

    def test_rejects_missing_host_accounting(self) -> None:
        host = "\n".join(
            line
            for line in HOST_TRACE.splitlines()
            if not line.startswith(("AXVISOR_RT_HOST_PCPU", "AXVISOR_RT_HOST_VCPU"))
        )
        temporary, host_path, guest_path = self.write_traces(host=host)
        with temporary, self.assertRaisesRegex(
            self.module.AnalysisError, "pCPU/vCPU accounting"
        ):
            self.module.analyze_irq_traces(host_path, guest_path)


if __name__ == "__main__":
    unittest.main()
