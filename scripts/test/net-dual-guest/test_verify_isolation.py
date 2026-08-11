#!/usr/bin/env python3
"""Deterministic tests for the P2 isolation evidence parser."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("verify_isolation.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("verify_isolation", MODULE_PATH)
assert SPEC and SPEC.loader
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


class VerifyIsolationTests(unittest.TestCase):
    def test_identity_mapping_and_stage2_mmio_are_observable(self) -> None:
        log = """
        [  1.0 axaddrspace] map_linear: [GPA:0x80000000, GPA:0xa0000000) -> [PA:0x80000000, PA:0xa0000000)
        [  1.1 axaddrspace] map_linear: [GPA:0xa0000000, GPA:0xc0000000) -> [PA:0xa0000000, PA:0xc0000000)
        [  1.2 axvm] VM[1] stage2 Passthrough: [0xa003000, 0xa004000) -> [0xa003000, 0xa004000)
        [  1.3 axvm] VM[2] stage2 Passthrough: [0xa003c00, 0xa004c00) -> [0xa003c00, 0xa004c00)
        [  1.4 gic] registered assigned AArch64 SPI route host_intid=79 guest_intid=79
        [  1.5 gic] registered assigned AArch64 SPI route host_intid=78 guest_intid=78
        """
        self.assertTrue(
            VERIFY.has_identity_memory_mapping(
                log, 0x80000000, 0xA0000000, 0x80000000, 0xA0000000
            )
        )
        self.assertTrue(
            VERIFY.has_stage2_mmio_mapping(log, 1, 0xA003E00, 0xA003E00, 0x200)
        )
        self.assertTrue(VERIFY.has_irq_route(log, 78))

    def test_identity_mapping_does_not_accept_missing_runtime_map(self) -> None:
        self.assertFalse(
            VERIFY.has_identity_memory_mapping(
                "map_linear: [GPA:0x80000000, GPA:0xa0000000) -> [PA:0x90000000, PA:0xb0000000)",
                0x80000000,
                0xA0000000,
                0x80000000,
                0xA0000000,
            )
        )


if __name__ == "__main__":
    unittest.main()
