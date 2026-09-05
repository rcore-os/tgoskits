"""Contract tests for the Starry nixosTest serial evaluator and proxy."""

from __future__ import annotations

import importlib.util
import queue
import unittest
from pathlib import Path
from types import SimpleNamespace


MODULE_PATH = Path(__file__).with_name("starry_machine.py")
SPEC = importlib.util.spec_from_file_location("starry_machine", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
STARRY_MACHINE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STARRY_MACHINE)


BOOT_CONSOLE = """
STARRY_NIXOS_PHASE=pid1
STARRY_NIXOS_PHASE=activation
STARRY_NIXOS_PHASE=systemd
STARRY_NIXOS_PHASE=marker
STARRY_NIXOS_SYSTEM_PASSED
"""

ASSERT_PASS_CONSOLE = BOOT_CONSOLE + """
STARRY_NIXOS_ASSERT_BEGIN
STARRY_NIXOS_ASSERT_CMD=hello
STARRY_NIXOS_ASSERT_STATUS=0
STARRY_NIXOS_ASSERT_OUTPUT_BEGIN
Hello, world!
STARRY_NIXOS_ASSERT_OUTPUT_END
STARRY_NIXOS_ASSERT_PASSED
"""

ASSERT_FAIL_CONSOLE = BOOT_CONSOLE + """
STARRY_NIXOS_ASSERT_BEGIN
STARRY_NIXOS_ASSERT_CMD=false
STARRY_NIXOS_ASSERT_STATUS=1
STARRY_NIXOS_ASSERT_OUTPUT_BEGIN
STARRY_NIXOS_ASSERT_OUTPUT_END
STARRY_NIXOS_ASSERT_FAILED:declared command false exited 1
"""

JOURNAL_PREFIXED_ASSERT_CONSOLE = BOOT_CONSOLE + """
starry-nixos-service-assert-start[2]: STARRY_NIXOS_ASSERT_BEGIN
starry-nixos-service-assert-start[2]: STARRY_NIXOS_ASSERT_CMD=hello
starry-nixos-service-assert-start[2]: STARRY_NIXOS_ASSERT_STATUS=0
starry-nixos-service-assert-start[2]: STARRY_NIXOS_ASSERT_OUTPUT_BEGIN
starry-nixos-service-assert-start[2]: Hello, world!
starry-nixos-service-assert-start[2]: STARRY_NIXOS_ASSERT_OUTPUT_END
starry-nixos-service-assert-start[2]: STARRY_NIXOS_ASSERT_PASSED
"""


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds


class ScriptedConsole:
    def __init__(self, snapshots: list[str]) -> None:
        self.snapshots = snapshots
        self.last_lines: queue.Queue[str] = queue.Queue()
        self.process = None
        self.calls = 0

    def get_console_log(self) -> str:
        index = min(self.calls, len(self.snapshots) - 1)
        self.calls += 1
        return self.snapshots[index]



class RecordingInner:
    def __init__(self) -> None:
        self.connected = False
        self.connect_called = False

    def connect(self) -> None:
        self.connect_called = True
        self.connected = True

    def succeed(self, command: str) -> str:
        self.connect()
        return command

    def start(self) -> None:
        return None


class StarryMachineTests(unittest.TestCase):
    def test_parses_passed_assertion_record(self) -> None:
        record = STARRY_MACHINE.parse_assertion_record(ASSERT_PASS_CONSOLE)
        self.assertEqual(record["result"], "Passed")
        self.assertEqual(record["command"], "hello")
        self.assertEqual(record["status"], 0)
        self.assertEqual(record["output"], "Hello, world!")

    def test_parses_journal_prefixed_assertion_record(self) -> None:
        record = STARRY_MACHINE.parse_assertion_record(JOURNAL_PREFIXED_ASSERT_CONSOLE)
        self.assertEqual(record["result"], "Passed")
        self.assertEqual(record["command"], "hello")
        self.assertEqual(record["status"], 0)
        self.assertEqual(record["output"], "Hello, world!")
        STARRY_MACHINE.evaluate_service_assertion(
            JOURNAL_PREFIXED_ASSERT_CONSOLE,
            expected_status=0,
            expected_output="Hello, world!",
            require_pass=True,
        )

    def test_parses_failed_assertion_record(self) -> None:
        record = STARRY_MACHINE.parse_assertion_record(ASSERT_FAIL_CONSOLE)
        self.assertEqual(record["result"], "Failed")
        self.assertEqual(record["reason"], "declared command false exited 1")

    def test_service_pass_requires_expected_output(self) -> None:
        record = STARRY_MACHINE.evaluate_service_assertion(
            ASSERT_PASS_CONSOLE,
            expected_status=0,
            expected_output="Hello, world!",
            require_pass=True,
        )
        self.assertEqual(record["status"], 0)

    def test_service_fail_rejects_unexpected_pass(self) -> None:
        with self.assertRaises(STARRY_MACHINE.StarryNixosTestError) as raised:
            STARRY_MACHINE.evaluate_service_assertion(
                ASSERT_PASS_CONSOLE,
                require_pass=False,
            )
        self.assertIn("STARRY_NIXOS_PHASE_FAILED=guest-assertion", str(raised.exception))

    def test_negative_assertion_is_guest_assertion_phase(self) -> None:
        record = STARRY_MACHINE.evaluate_service_assertion(
            ASSERT_FAIL_CONSOLE,
            require_pass=False,
        )
        self.assertEqual(record["result"], "Failed")
        with self.assertRaises(STARRY_MACHINE.StarryNixosTestError) as raised:
            STARRY_MACHINE.evaluate_service_assertion(
                ASSERT_FAIL_CONSOLE,
                require_pass=True,
            )
        self.assertIn("STARRY_NIXOS_PHASE_FAILED=guest-assertion", str(raised.exception))
        self.assertIn("declared command false exited 1", str(raised.exception))

    def test_boot_timeout_and_startup_phases(self) -> None:
        with self.assertRaises(STARRY_MACHINE.StarryNixosTestError) as timeout:
            STARRY_MACHINE.evaluate_boot_console(
                "early boot noise",
                terminal_seen=False,
                qemu_exited=False,
            )
        self.assertIn("STARRY_NIXOS_PHASE_FAILED=timeout", str(timeout.exception))

        with self.assertRaises(STARRY_MACHINE.StarryNixosTestError) as startup:
            STARRY_MACHINE.evaluate_boot_console(
                "",
                terminal_seen=False,
                qemu_exited=True,
            )
        self.assertIn("STARRY_NIXOS_PHASE_FAILED=machine-startup", str(startup.exception))

        with self.assertRaises(STARRY_MACHINE.StarryNixosTestError) as activation:
            STARRY_MACHINE.evaluate_boot_console(
                "STARRY_NIXOS_PHASE=pid1",
                terminal_seen=False,
                qemu_exited=False,
            )
        self.assertIn("STARRY_NIXOS_PHASE_FAILED=stage2-activation", str(activation.exception))

        with self.assertRaises(STARRY_MACHINE.StarryNixosTestError) as panic:
            STARRY_MACHINE.evaluate_boot_console(
                "kernel panic on cpu 0",
                terminal_seen=True,
                qemu_exited=True,
            )
        self.assertIn("STARRY_NIXOS_PHASE_FAILED=unexpected-guest-exit", str(panic.exception))

    def test_boot_success_console_is_accepted(self) -> None:
        STARRY_MACHINE.evaluate_boot_console(
            BOOT_CONSOLE,
            terminal_seen=True,
            qemu_exited=False,
        )

    def test_unsupported_succeed_does_not_connect(self) -> None:
        inner = RecordingInner()
        machine = STARRY_MACHINE.wrap_machine(inner)
        with self.assertRaises(STARRY_MACHINE.StarryNixosTestError) as raised:
            machine.succeed("true")
        self.assertFalse(inner.connect_called)
        self.assertFalse(inner.connected)
        message = str(raised.exception)
        self.assertIn("unsupported Starry nixosTest operation: succeed", message)
        self.assertIn("STARRY_NIXOS_PHASE_FAILED=guest-assertion", message)

    def test_unsupported_execute_and_wait_for_unit(self) -> None:
        inner = RecordingInner()
        machine = STARRY_MACHINE.wrap_machine(inner)
        with self.assertRaises(STARRY_MACHINE.StarryNixosTestError):
            machine.execute("true")
        with self.assertRaises(STARRY_MACHINE.StarryNixosTestError):
            machine.wait_for_unit("multi-user.target")
        self.assertFalse(inner.connect_called)

    def test_allowed_start_reaches_inner(self) -> None:
        inner = SimpleNamespace(started=False)

        def start() -> None:
            inner.started = True

        inner.start = start
        STARRY_MACHINE.wrap_machine(inner).start()
        self.assertTrue(inner.started)

    def test_wait_for_assertion_continues_after_system_passed(self) -> None:
        clock = FakeClock()
        machine = ScriptedConsole([BOOT_CONSOLE, BOOT_CONSOLE, ASSERT_PASS_CONSOLE])
        console, seen, qemu_exited = STARRY_MACHINE.wait_for_console_evidence(
            machine,
            r"STARRY_NIXOS_ASSERT_PASSED|STARRY_NIXOS_ASSERT_FAILED:",
            deadline=clock.now + 10,
            now=clock,
            sleep=clock.sleep,
        )
        self.assertTrue(seen)
        self.assertFalse(qemu_exited)
        self.assertIn("STARRY_NIXOS_ASSERT_PASSED", console)
        self.assertGreaterEqual(machine.calls, 3)


if __name__ == "__main__":
    unittest.main()
