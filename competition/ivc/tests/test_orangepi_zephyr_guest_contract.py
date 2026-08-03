#!/usr/bin/env python3

import re
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
ORANGEPI_ZEPHYR_CONFIGS = (
    REPOSITORY_ROOT / "competition/ivc/config/orangepi-5-plus-zephyr-smp1.toml",
    REPOSITORY_ROOT / "competition/ivc/config/orangepi-5-plus-zephyr-smoke.toml",
)
ZEPHYR_MAIN = REPOSITORY_ROOT / "competition/ivc/zephyr/src/main.c"


class OrangePiZephyrGuestContractTests(unittest.TestCase):
    def test_emulated_device_guests_use_virtual_interrupt_delivery(self) -> None:
        for config_path in ORANGEPI_ZEPHYR_CONFIGS:
            with self.subTest(config=config_path.name):
                config = config_path.read_text(encoding="utf-8")

                self.assertRegex(config, r"(?m)^passthrough_devices\s*=\s*\[\s*\]$")
                self.assertRegex(
                    config,
                    re.compile(r'(?m)^interrupt_mode\s*=\s*"emulated"$'),
                )
                self.assertRegex(
                    config,
                    re.compile(r"(?m)^aarch64_virtual_timer_irq\s*=\s*27$"),
                )

    def test_terminal_evidence_uses_redundant_compact_records(self) -> None:
        source = ZEPHYR_MAIN.read_text(encoding="utf-8")

        self.assertRegex(
            source,
            r"(?m)^#define IVC_RESULT_RECORD_COPIES 2U$",
        )
        self.assertIn("IVC-RTOS-OUTCOME profile=%s", source)
        self.assertIn("IVC-RTOS-MESSAGES status_sent=%llu", source)


if __name__ == "__main__":
    unittest.main()
