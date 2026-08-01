from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType, SimpleNamespace


def load_tool() -> ModuleType:
    path = Path(__file__).with_name("board_power.py")
    spec = importlib.util.spec_from_file_location("board_power", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


board_power = load_tool()


class FakeStatus:
    def __init__(self, power_on: bool) -> None:
        self.power_on = power_on

    def property_dict(self):
        return {
            "switch:on": SimpleNamespace(
                value=self.power_on,
                service=SimpleNamespace(siid=2),
                piid=1,
            ),
            "switch:fault": SimpleNamespace(value=0),
            "power-consumption:electric-power": SimpleNamespace(value=3),
            "on-off-count:temperature": SimpleNamespace(value=42),
        }


class FakeDevice:
    def __init__(self, power_on: bool = True) -> None:
        self.power_on = power_on
        self.transitions: list[bool] = []

    def status(self):
        return FakeStatus(self.power_on)

    def set_property_by(
        self,
        siid: int,
        piid: int,
        value: bool,
        *,
        name: str,
    ):
        if (siid, piid, name) != (2, 1, "switch:on"):
            raise AssertionError((siid, piid, name))
        self.power_on = value
        self.transitions.append(value)
        return [{"code": 0}]


class BoardPowerTests(unittest.TestCase):
    def test_load_config_prefers_environment_token(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "power.toml"
            path.write_text(
                """
[board_power]
ip = "192.168.31.183"
token = "00000000000000000000000000000000"
""".strip(),
                encoding="utf-8",
            )

            config = board_power.load_config(
                path,
                {"TGOS_BOARD_POWER_TOKEN": "11111111111111111111111111111111"},
            )

        self.assertEqual(config.token, "11111111111111111111111111111111")

    def test_off_requires_explicit_confirmation(self) -> None:
        with self.assertRaisesRegex(board_power.PowerControlError, "requires --yes"):
            board_power.ensure_action_is_authorized("off", False)

    def test_cycle_restores_power_after_off_interval(self) -> None:
        device = FakeDevice()
        sleeps: list[float] = []
        config = board_power.PlugConfig(
            name="orangepi-5-plus-rk3588",
            ip="192.168.31.183",
            model="cuco.plug.v3",
            token="1" * 32,
            off_seconds=8,
            timeout_seconds=5,
        )

        status = board_power.execute_action(
            config,
            "cycle",
            device_factory=lambda _: device,
            sleep=sleeps.append,
        )

        self.assertTrue(status.power_on)
        self.assertEqual(device.transitions, [False, True])
        self.assertIn(8, sleeps)

    def test_on_is_rejected_when_plug_reports_fault(self) -> None:
        device = FakeDevice(power_on=False)
        device.status = lambda: SimpleNamespace(
            property_dict=lambda: {
                "switch:on": SimpleNamespace(value=False),
                "switch:fault": SimpleNamespace(value=2),
            },
        )
        config = board_power.PlugConfig(
            name="orangepi-5-plus-rk3588",
            ip="192.168.31.183",
            model="cuco.plug.v3",
            token="1" * 32,
            off_seconds=8,
            timeout_seconds=5,
        )

        with self.assertRaisesRegex(board_power.PowerControlError, "fault code 2"):
            board_power.execute_action(
                config,
                "on",
                device_factory=lambda _: device,
                sleep=lambda _: None,
            )

        self.assertEqual(device.transitions, [])


if __name__ == "__main__":
    unittest.main()
