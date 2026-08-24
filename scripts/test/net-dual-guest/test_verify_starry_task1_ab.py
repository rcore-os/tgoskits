#!/usr/bin/env python3
"""Deterministic tests for the StarryOS Task 1 A/B evidence verifier."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("verify_starry_task1_ab.py")
CONFIG_DIR = MODULE_PATH.parent
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("verify_starry_task1_ab", MODULE_PATH)
assert SPEC and SPEC.loader
VERIFY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = VERIFY
SPEC.loader.exec_module(VERIFY)


class VerifyStarryTask1AbTests(unittest.TestCase):
    def test_repository_task1_configs_define_the_required_single_variable_ab(self) -> None:
        self.assertEqual(
            VERIFY.verify_host_config_pair(
                CONFIG_DIR / "axvisor-qemu-starry-task1-rr.toml",
                CONFIG_DIR / "axvisor-qemu-starry-task1-fp-rr.toml",
            ),
            [],
        )
        self.assertEqual(
            VERIFY.verify_shared_pcpu_configs(
                CONFIG_DIR / "vm-aarch64-starry-task1-shared.toml",
                CONFIG_DIR / "vm-aarch64-zephyr-task1-shared.toml",
                "zephyr",
            ),
            [],
        )

    def test_host_configs_may_differ_only_by_scheduler_feature(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            rr = root / "rr.toml"
            fp_rr = root / "fp-rr.toml"
            rr.write_text(
                'features = ["rr-scheduler", "fs"]\nlog = "Info"\ntarget = "aarch64"\n'
            )
            fp_rr.write_text(
                'features = ["fp-rr-scheduler", "fs"]\nlog = "Info"\ntarget = "aarch64"\n'
            )

            self.assertEqual(VERIFY.verify_host_config_pair(rr, fp_rr), [])

            fp_rr.write_text(
                'features = ["fp-rr-scheduler", "fs", "extra"]\n'
                'log = "Info"\ntarget = "aarch64"\n'
            )
            self.assertTrue(VERIFY.verify_host_config_pair(rr, fp_rr))

    def test_fp_rr_log_requires_nonzero_bounded_service_counter(self) -> None:
        zero_log = "\n".join(
            (
                "use Fixed-priority round-robin scheduler.",
                "FP-RR scheduler counters:",
                "lower_priority_services=0",
            )
        )
        failures, count = VERIFY.verify_scheduler_log("fp-rr", zero_log)
        self.assertEqual(count, 0)
        self.assertTrue(failures)

        active_log = zero_log.replace("services=0", "services=7")
        self.assertEqual(VERIFY.verify_scheduler_log("fp-rr", active_log), ([], 7))

    def test_rtt_summary_uses_nearest_rank_p95(self) -> None:
        summary = VERIFY.summarize_rtts([71, 215, 86, 75])

        self.assertEqual(summary.count, 4)
        self.assertEqual(summary.minimum, 71)
        self.assertEqual(summary.median, 80.5)
        self.assertEqual(summary.p95, 215)
        self.assertEqual(summary.maximum, 215)

    def test_guest_artifact_hashes_must_match(self) -> None:
        hashes = {role: f"hash-{role}" for role in VERIFY.GUEST_ARTIFACT_SUFFIXES}
        hashes["zephyr"] = "hash-zephyr"
        self.assertEqual(
            VERIFY.verify_guest_artifact_equivalence(hashes, hashes, "zephyr"), []
        )

        changed = dict(hashes)
        changed["zephyr"] = "different"
        self.assertTrue(
            VERIFY.verify_guest_artifact_equivalence(hashes, changed, "zephyr")
        )

    def test_rootfs_content_must_match_endpoint_script_and_yolo_assets(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            hashes = Path(temporary) / "rootfs-content-hashes.txt"
            pairs = {
                "endpoint": "endpoint",
                "script": "script",
                "yolo_param": "param",
                "yolo_model": "model",
                "yolo_input": "input",
            }
            hashes.write_text(
                "".join(
                    f"host_{name}_sha256={digest}\nrootfs_{name}_sha256={digest}\n"
                    for name, digest in pairs.items()
                )
            )
            self.assertEqual(VERIFY.verify_rootfs_content_hashes(hashes), [])

            hashes.write_text(
                hashes.read_text().replace(
                    "rootfs_script_sha256=script", "rootfs_script_sha256=stale"
                )
            )
            self.assertTrue(VERIFY.verify_rootfs_content_hashes(hashes))


if __name__ == "__main__":
    unittest.main()
