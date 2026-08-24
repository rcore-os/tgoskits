#!/usr/bin/env python3
"""Unit regressions for the serial console experiment driver."""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


SCRIPT = Path(__file__).with_name("serial_console.py")
SPEC = importlib.util.spec_from_file_location("serial_console", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class FakeConnection:
    def __init__(self):
        self.sent = []

    def sendall(self, payload):
        self.sent.append(payload)


class ChunkedConnection:
    def __init__(self, chunks):
        self.chunks = iter(chunks)

    def recv(self, _size):
        try:
            return next(self.chunks)
        except StopIteration as error:
            raise MODULE.socket.timeout from error


class FakeQmpSession:
    commands = []

    def __init__(self, _sock_path):
        pass

    def __enter__(self):
        return self

    def __exit__(self, _exc_type, _exc, _traceback):
        return False

    def execute(self, command, arguments=None):
        self.commands.append((command, arguments))
        return {"return": {"command": command, "arguments": arguments}}


class SerialConsoleTest(unittest.TestCase):
    def test_poll_reads_tracks_attachment_marker_split_across_receives(self):
        driver = object.__new__(MODULE.ConsoleDriver)
        driver.conn = ChunkedConnection(
            [b"[Axvisor] attached VM[", b"2] console; use Ctrl+X\n"]
        )
        driver.closed = False
        driver.dumping = False
        driver.dump_lines = []
        driver.tail = b""
        driver.attached = False
        driver.last_vm = None
        driver.write_log = mock.Mock()
        driver.observe_progress = mock.Mock()

        with mock.patch.object(MODULE.sys, "stdout"):
            driver.poll_reads()

        self.assertTrue(driver.attached)
        self.assertEqual(driver.last_vm, 2)

    def test_send_until_retries_gate_byte_until_marker_arrives(self):
        driver = object.__new__(MODULE.ConsoleDriver)
        driver.conn = FakeConnection()
        driver.closed = False
        driver.watchdog_error = None
        outcomes = iter([False, False, True])
        driver.wait_for = lambda _pattern, _seconds: next(outcomes)

        observed = driver.send_until(b"g", "PERIODIC LATENCY START", 5.0, 0.1)

        self.assertTrue(observed)
        self.assertEqual(driver.conn.sent, [b"g", b"g", b"g"])

    def test_watchdog_failure_collects_forensics_before_returning(self):
        driver = object.__new__(MODULE.ConsoleDriver)
        driver.watchdog_error = "stalled"
        driver.collect_forensics = mock.Mock()
        args = SimpleNamespace(qmp_sock="qmp.sock", forensics_dir="artifacts")

        status = MODULE.report_watchdog_failure(driver, args)

        self.assertEqual(status, 4)
        driver.collect_forensics.assert_called_once_with("qmp.sock", "artifacts")

    def test_expectation_failure_collects_forensics_before_returning(self):
        driver = object.__new__(MODULE.ConsoleDriver)
        driver.collect_forensics = mock.Mock()
        args = SimpleNamespace(qmp_sock="qmp.sock", forensics_dir="artifacts")

        status = MODULE.report_expectation_failure(driver, args, "missing gate")

        self.assertEqual(status, 2)
        driver.collect_forensics.assert_called_once_with("qmp.sock", "artifacts")

    def test_qmp_forensics_persists_each_requested_snapshot(self):
        FakeQmpSession.commands = []
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(MODULE, "QmpSession", FakeQmpSession):
                MODULE.collect_qmp_forensics("qmp.sock", directory)

            artifact_dir = Path(directory)
            expected = {
                "query-status.json",
                "query-cpus-fast.json",
                "query-chardev.json",
                "info-registers-1.json",
                "info-registers-2.json",
            }
            self.assertEqual(
                {path.name for path in artifact_dir.glob("*.json")}, expected
            )
            for name in expected:
                payload = json.loads((artifact_dir / name).read_text())
                self.assertIn("return", payload)

        self.assertEqual(
            FakeQmpSession.commands,
            [
                ("query-status", None),
                ("query-cpus-fast", None),
                ("query-chardev", None),
                (
                    "human-monitor-command",
                    {"command-line": "info registers -a"},
                ),
                (
                    "human-monitor-command",
                    {"command-line": "info registers -a"},
                ),
            ],
        )

    def test_dump_pcap_streams_guest_records_and_writes_both_files(self):
        driver = object.__new__(MODULE.ConsoleDriver)
        driver.conn = FakeConnection()
        driver.closed = False
        driver.dump_lines = []

        def fake_poll_reads():
            driver.dump_lines = [
                "CAPDUMP_BEGIN\n"
                "CAPTURE 1 100 0011\n"
                "CAPTURE 2 200 aabb\n"
                "CAPDUMP_END\n"
            ]

        driver.poll_reads = fake_poll_reads
        with tempfile.TemporaryDirectory() as directory:
            driver.dump_pcap(str(Path(directory) / "switch"))
            vm1 = Path(directory) / "switch.vm1.pcap"
            vm2 = Path(directory) / "switch.vm2.pcap"
            self.assertEqual(driver.conn.sent, [b"virtnet capture dump\n"])
            self.assertEqual(vm1.read_bytes()[:4], MODULE.PCAP_GLOBAL_HEADER[:4])
            self.assertEqual(vm2.read_bytes()[:4], MODULE.PCAP_GLOBAL_HEADER[:4])
            self.assertGreater(len(vm1.read_bytes()), len(MODULE.PCAP_GLOBAL_HEADER))
            self.assertGreater(len(vm2.read_bytes()), len(MODULE.PCAP_GLOBAL_HEADER))


if __name__ == "__main__":
    unittest.main()
