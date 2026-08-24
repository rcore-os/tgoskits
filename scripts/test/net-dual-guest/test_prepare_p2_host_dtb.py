#!/usr/bin/env python3
"""Regression tests for deterministic P2 host-DTB preparation."""

from __future__ import annotations

import os
import stat
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
PREPARE_SCRIPT = SCRIPT_DIR / "prepare-p2-host-dtb.sh"


def minimal_dtb() -> bytes:
    """Build the smallest FDT accepted by the carveout helper."""
    header_size = 40
    reservation_size = 16
    structure_size = 4
    total_size = header_size + reservation_size + structure_size
    header = struct.pack(
        ">10I",
        0xD00D_FEED,
        total_size,
        header_size + reservation_size,
        total_size,
        header_size,
        17,
        16,
        0,
        0,
        structure_size,
    )
    return header + struct.pack(">2Q", 0, 0) + struct.pack(">I", 9)


class PrepareP2HostDtbTests(unittest.TestCase):
    def test_qemu_dtb_randomness_is_disabled(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temp_dir = Path(directory)
            template = temp_dir / "template.dtb"
            template.write_bytes(minimal_dtb())
            fake_qemu = temp_dir / "fake-qemu"
            fake_qemu.write_text(
                """#!/usr/bin/env bash
set -euo pipefail
machine=""
while (($#)); do
    if [[ "$1" == "-machine" ]]; then
        machine="$2"
        break
    fi
    shift
done
[[ "$machine" == *",dtb-randomness=off"* ]]
output="${machine#*dumpdtb=}"
output="${output%%,*}"
cp "$TASK2_FAKE_DTB_TEMPLATE" "$output"
""",
                encoding="utf-8",
            )
            fake_qemu.chmod(fake_qemu.stat().st_mode | stat.S_IXUSR)

            environment = os.environ.copy()
            environment.update(
                {
                    "OUT_DIR": str(temp_dir / "output"),
                    "QEMU_AARCH64": str(fake_qemu),
                    "TASK2_FAKE_DTB_TEMPLATE": str(template),
                }
            )
            result = subprocess.run(
                ["bash", str(PREPARE_SCRIPT)],
                check=False,
                env=environment,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
