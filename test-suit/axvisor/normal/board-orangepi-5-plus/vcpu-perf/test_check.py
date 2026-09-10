#!/usr/bin/env python3
"""Exercise the performance verdict, not a simulated hypervisor."""
import contextlib
import io
import unittest

from check import evaluate


def log(blocks=3000, wakes=300, indexes=range(6), done=True):
    text = "VCPU_PERF_LOAD_READY cpu=0\n"
    text += "".join(f"VCPU_PERF_SAMPLE index={i} blocks={blocks} elapsed_ns=3000000000 timer_wakes={wakes} checksum=7\n" for i in indexes)
    return text + ("VCPU_PERF_DONE windows=5\n" if done else "")


class VerdictTests(unittest.TestCase):
    def setUp(self):
        self.config = dict(baseline_blocks_per_second=1000, max_regression_percent=15, min_timer_wakes_per_second=50)

    def score(self, text, **kwargs):
        with contextlib.redirect_stdout(io.StringIO()):
            return evaluate(text, self.config, **kwargs)

    def test_throughput_budget_boundary(self):
        self.assertEqual(self.score(log(blocks=2550)), 850)
        with self.assertRaisesRegex(ValueError, "throughput regressed"):
            self.score(log(blocks=2549))

    def test_guest_console_tag_does_not_change_verdict(self):
        tagged = log().replace("VCPU_PERF_SAMPLE", "[VM 1] VCPU_PERF_SAMPLE").replace(
            "VCPU_PERF_DONE", "[VM 1] VCPU_PERF_DONE"
        )
        self.assertEqual(self.score(tagged), self.score(log()))
        with self.assertRaises(ValueError):
            self.score(tagged.replace("[VM 1]", "[VM 2]"))

    def test_missing_work_cannot_pass_as_fast(self):
        for text in [log(wakes=0), log(done=False), log(indexes=[0, 1, 2, 3, 4]), log(indexes=[0, 1, 2, 3, 4, 4, 5]), log().replace("VCPU_PERF_LOAD_READY cpu=0\n", ""), log(blocks=0)]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                self.score(text)

    def test_early_load_exit_is_not_a_valid_sample(self):
        with self.assertRaises(ValueError):
            self.score(log().replace("VCPU_PERF_SAMPLE index=2", "VCPU_PERF_LOAD_STOPPED\nVCPU_PERF_SAMPLE index=2"))

    def test_completion_must_be_unique_and_follow_work(self):
        for text in [log() + "VCPU_PERF_DONE windows=5\n", "VCPU_PERF_DONE windows=5\n" + log(done=False)]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                self.score(text)

    def test_unqualified_baseline_never_passes_gate(self):
        self.config["baseline_blocks_per_second"] = 0
        with self.assertRaisesRegex(ValueError, "not been qualified"):
            self.score(log())
        self.assertEqual(self.score(log(), measure=True), 1000)

    def test_invalid_numeric_configuration(self):
        for value in [float("nan"), float("inf"), -1, True]:
            with self.subTest(value=value):
                self.config["baseline_blocks_per_second"] = value
                with self.assertRaises(ValueError):
                    self.score(log())


if __name__ == "__main__":
    unittest.main()
