"""Exercise pasted input through a real picocom process and two PTYs."""

import io
import os
from pathlib import Path
import re
import select
import shutil
import subprocess
import sys
import tempfile
import time
import tty
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import serial_selfbuild


CONNECT_SERIAL = Path(__file__).resolve().parents[1] / "connect_serial.sh"


class SelfbuildDriverTests(unittest.TestCase):
    def test_kernel_build_command_is_sent_once_and_terminal_status_is_preserved(self):
        class SerialInput:
            def __init__(self, prompt, ending):
                self.chunks = [prompt[:10], prompt[10:], ending]
                self.commands = []

            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

            @property
            def in_waiting(self):
                return len(self.chunks[0]) if self.chunks else 0

            def read(self, size):
                return self.chunks.pop(0) if self.chunks else b""

            def write(self, command):
                self.commands.append(command)

            def flush(self):
                pass

        root = CONNECT_SERIAL.parents[3]
        temporary_root = root / "tmp"
        temporary_root.mkdir(exist_ok=True)
        cases = [
            (prompt, ending, expected_status)
            for prompt in [b"root@starry:/root #", b"root@starry:~# "]
            for ending, expected_status in [
                (b"===STARRY-ORANGEPI5PLUS-SELFBUILD-PASS run=cold elapsed=900===\r\n", 0),
                (b"===STARRY-ORANGEPI5PLUS-SELFBUILD-FAIL rc=1===\r\n", 1),
                (b"", 1),
            ]
        ]
        for prompt, ending, expected_status in cases:
            with (
                self.subTest(prompt=prompt, ending=ending),
                tempfile.TemporaryDirectory(dir=temporary_root) as temporary,
            ):
                uart = SerialInput(prompt, ending + prompt)
                directory = Path(temporary)
                arguments = [
                    "serial_selfbuild.py", "--serial", "fake-uart", "--log",
                    str(directory / "serial.log"), "--ready-file",
                    str(directory / "ready"), "--run-id", "cold",
                    "--kernel-only", "--timeout", "1",
                ]
                with io.TextIOWrapper(io.BytesIO()) as output:
                    with (
                        patch.object(sys, "argv", arguments),
                        patch.object(sys, "stdout", output),
                        patch.object(serial_selfbuild.serial, "Serial", return_value=uart),
                    ):
                        status = serial_selfbuild.main()
                self.assertEqual(status, expected_status)
                self.assertEqual(uart.commands, [
                    b"sh /opt/starry-orangepi5plus-selfbuild/init-kernel-selfbuild.sh cold\r"
                ])


def read_until(descriptor, marker, timeout=5):
    received = bytearray()
    deadline = time.monotonic() + timeout
    while marker not in received and time.monotonic() < deadline:
        ready, _, _ = select.select([descriptor], [], [], 0.1)
        if ready:
            received.extend(os.read(descriptor, 65536))
    if marker not in received:
        raise AssertionError(f"did not receive {marker!r}: {bytes(received)!r}")
    return bytes(received)


@unittest.skipUnless(shutil.which("picocom"), "picocom is required for the PTY test")
class SerialConsoleTests(unittest.TestCase):
    def test_paste_does_not_send_terminal_wrapper_bytes_to_serial(self):
        terminal_master, terminal_slave = os.openpty()
        serial_master, serial_slave = os.openpty()
        process = None
        try:
            tty.setraw(serial_slave)
            process = subprocess.Popen(
                ["bash", str(CONNECT_SERIAL), os.ttyname(serial_slave)],
                stdin=terminal_slave,
                stdout=terminal_slave,
                stderr=terminal_slave,
                start_new_session=True,
            )
            output = read_until(terminal_master, b"Terminal ready")

            # Model a terminal whose previous application enabled bracketed
            # paste. Its renderer applies mode changes before encoding a paste.
            bracketed_paste = True
            for mode in re.findall(rb"\x1b\[\?2004([hl])", output):
                bracketed_paste = mode == b"h"
            pasted_command = b"echo SERIAL_PASTE_OK\r"
            keyboard_input = pasted_command
            if bracketed_paste:
                keyboard_input = b"\x1b[200~" + pasted_command + b"\x1b[201~"
            os.write(terminal_master, keyboard_input)
            received = read_until(serial_master, pasted_command)
            self.assertEqual(received, pasted_command)
        finally:
            if process is not None and process.poll() is None:
                os.write(terminal_master, b"\x01\x18")
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            for descriptor in (terminal_master, terminal_slave, serial_master, serial_slave):
                os.close(descriptor)


if __name__ == "__main__":
    unittest.main(verbosity=2)
