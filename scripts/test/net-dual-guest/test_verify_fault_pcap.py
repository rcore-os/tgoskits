#!/usr/bin/env python3
"""Deterministic tests for P3 fault evidence handling."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("verify_fault_pcap.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("verify_fault_pcap", MODULE_PATH)
assert SPEC and SPEC.loader
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


class VerifyFaultPcapTests(unittest.TestCase):
    def test_interleaved_guest_serial_fragments_are_searchable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "axvisor.log"
            log.write_text(
                "[VM 1] TASK2_RETRANS\n"
                "[  1.0 host] unrelated interrupt log\n"
                "MIT seq=1 attempt=1\n"
                "[VM 1] TASK2_DUPLICATE seq=1\n"
                "[VM 1] TASK2_ACK seq=1\n",
                encoding="utf-8",
            )
            self.assertEqual(
                VERIFY.require_log_patterns(
                    log,
                    (r"TASK2_RETRANS[\s\S]{0,512}MIT\b", r"TASK2_DUPLICATE\b", r"TASK2_ACK\b"),
                ),
                [],
            )


if __name__ == "__main__":
    unittest.main()
