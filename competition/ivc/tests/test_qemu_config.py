from __future__ import annotations

import re
try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 and older
    import tomli as tomllib
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
QEMU_CONFIG = REPOSITORY_ROOT / "competition/ivc/config/qemu-aarch64.toml"
ORANGEPI_BOARD_CONFIGS = (
    REPOSITORY_ROOT / "competition/ivc/config/board-orangepi-5-plus-smoke.toml",
    REPOSITORY_ROOT / "competition/ivc/config/board-orangepi-5-plus.toml",
)
LINUX_ACK_LOSS_CONFIG = (
    REPOSITORY_ROOT / "competition/ivc/config/linux-smp2-ack-loss.toml"
)
ZEPHYR_ACK_LOSS_CONFIG = (
    REPOSITORY_ROOT / "competition/ivc/config/zephyr-smp1-ack-loss.toml"
)
ZEPHYR_ACK_LOSS_CONF = REPOSITORY_ROOT / "competition/ivc/zephyr/ack-loss.conf"
ZEPHYR_KCONFIG = REPOSITORY_ROOT / "competition/ivc/zephyr/Kconfig"
ZEPHYR_GITIGNORE = REPOSITORY_ROOT / "competition/ivc/zephyr/.gitignore"


class QemuConfigContractTests(unittest.TestCase):
    def test_success_waits_for_complete_linux_result_line(self) -> None:
        with QEMU_CONFIG.open("rb") as source:
            config = tomllib.load(source)

        self.assertEqual(len(config["success_regex"]), 1)
        success = re.compile(config["success_regex"][0])
        failure_patterns = [re.compile(pattern) for pattern in config["fail_regex"]]
        prefix = (
            "IVC-CONTROLLER-RESULT policy=neural sent=1800 acknowledged=1800 "
            "errors=0 timeouts=0"
        )

        self.assertIsNone(success.search(prefix))
        self.assertIsNotNone(success.search("IVC-LINUX-DONE exit=0\n"))
        self.assertFalse(any(pattern.search(prefix) for pattern in failure_patterns))
        self.assertTrue(
            any(
                pattern.search(prefix.replace("errors=0", "errors=1"))
                for pattern in failure_patterns
            )
        )

    def test_orangepi_success_requires_a_complete_line_with_serial_crlf(self) -> None:
        completed = "[guest-console:pl011-linux] IVC-LINUX-DONE exit=0"

        for config_path in ORANGEPI_BOARD_CONFIGS:
            with self.subTest(config=config_path.name), config_path.open("rb") as source:
                config = tomllib.load(source)
            self.assertEqual(len(config["success_regex"]), 1)
            success = re.compile(config["success_regex"][0])

            self.assertIsNone(success.search(completed))
            self.assertIsNotNone(success.search(f"{completed}\n"))
            self.assertIsNotNone(success.search(f"{completed}\r\n"))
            self.assertIsNotNone(success.search(f"{completed}\r\r\n"))

    def test_ack_loss_guest_configs_pin_the_100_command_fault_campaign(self) -> None:
        with LINUX_ACK_LOSS_CONFIG.open("rb") as source:
            linux = tomllib.load(source)
        with ZEPHYR_ACK_LOSS_CONFIG.open("rb") as source:
            zephyr = tomllib.load(source)

        self.assertEqual(linux["base"]["cpu_num"], 2)
        self.assertEqual(linux["base"]["phys_cpu_sets"], [0x2, 0x4])
        self.assertIn("ivc.mode=neural", linux["kernel"]["cmdline"])
        self.assertIn("ivc.count=100", linux["kernel"]["cmdline"])
        self.assertIn("ivc.period_ms=100", linux["kernel"]["cmdline"])
        self.assertEqual(zephyr["base"]["phys_cpu_sets"], [0x1])
        self.assertEqual(
            zephyr["kernel"]["kernel_path"],
            "../zephyr/build-ack-loss/zephyr/zephyr.bin",
        )
        self.assertEqual(zephyr["devices"]["emu_devices"][0][4:], [0xE2, [2, 1, 1]])

    def test_ack_loss_build_overlay_is_explicit_and_default_remains_off(self) -> None:
        fault_config = ZEPHYR_ACK_LOSS_CONF.read_text(encoding="utf-8")
        kconfig = ZEPHYR_KCONFIG.read_text(encoding="utf-8")
        gitignore = ZEPHYR_GITIGNORE.read_text(encoding="utf-8").splitlines()

        self.assertIn("CONFIG_IVC_DROP_ACK_EVERY=5", fault_config)
        self.assertIn("CONFIG_IVC_EXPECTED_COMMANDS=100", fault_config)
        self.assertRegex(
            kconfig,
            r"(?s)config IVC_DROP_ACK_EVERY.*?default 0",
        )
        self.assertRegex(
            kconfig,
            r"(?s)config IVC_EXPECTED_COMMANDS.*?default 0",
        )
        self.assertIn("/build-ack-loss/", gitignore)


if __name__ == "__main__":
    unittest.main()
