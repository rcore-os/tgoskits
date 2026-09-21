"""Serial transport owned by the three-guest acceptance case."""
import codecs
import os
import re
import select
import shlex
import signal
import subprocess
import sys
import termios
import time
import tty


class ConsoleFilter:
    """Strip mouse capture/report sequences, preserving split CSI and colors."""
    def __init__(self):
        self.pending = b""

    def feed(self, chunk):
        self.pending += chunk
        output = bytearray()
        while self.pending:
            start = self.pending.find(b"\x1b")
            if start < 0:
                output.extend(self.pending); self.pending = b""; break
            output.extend(self.pending[:start]); self.pending = self.pending[start:]
            if self.pending == b"\x1b":
                break
            if not self.pending.startswith(b"\x1b["):
                output.append(self.pending[0]); self.pending = self.pending[1:]; continue
            match = re.match(rb"\x1b\[[0-?]*[ -/]*[@-~]", self.pending)
            if not match:
                if len(self.pending) <= 128:
                    break
                output.append(self.pending[0]); self.pending = self.pending[1:]; continue
            sequence = match[0]
            modes = sequence[3:-1].split(b";")
            capture = sequence.startswith(b"\x1b[?") and sequence[-1:] in (b"h", b"l") and any(
                mode in (b"1000", b"1002", b"1003", b"1006", b"1015") for mode in modes)
            if not capture and not sequence.startswith(b"\x1b[<"):
                output.extend(sequence)
            self.pending = self.pending[len(sequence):]
        return bytes(output)


class KeyboardGate:
    def __init__(self):
        self.ctrl_a = False

    def feed(self, data, interactive):
        output = bytearray()
        for byte in data:
            if self.ctrl_a:
                self.ctrl_a = False
                if byte == ord("x"):
                    return bytes(output) + b"\x01x", True
                if interactive:
                    output.append(1)
            if byte == 1:
                self.ctrl_a = True
            elif interactive:
                output.append(byte)
        return bytes(output), False


def say(text):
    sys.stdout.write(text.replace("\n", "\r\n") + "\r\n")
    sys.stdout.flush()


def run_console(command, demo, cwd):
    # script supplies the child PTY only; /dev/null discards its recording.
    process = subprocess.Popen(["script", "-q", "-e", "-c", shlex.join(command), "/dev/null"],
                               cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, start_new_session=True)
    saved = termios.tcgetattr(0) if os.isatty(0) else None
    output_filter, input_filter, gate = ConsoleFilter(), ConsoleFilter(), KeyboardGate()
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    ci = demo is not None and demo.ci
    pending, next_send, deadline = b"", 0.0, time.monotonic() + (1800 if ci else 300)
    exit_deadline = None
    inputs = [process.stdout, sys.stdin]
    disable_mouse = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l"
    try:
        if saved:
            tty.setraw(0)
            os.write(1, disable_mouse)
        while True:
            ready, _, _ = select.select(inputs, [], [], 0.03)
            if process.stdout in ready:
                chunk = os.read(process.stdout.fileno(), 4096)
                if not chunk:
                    break
                chunk = output_filter.feed(chunk)
                # Automatic and manual sessions use the same raw output path.
                sys.stdout.buffer.write(chunk); sys.stdout.buffer.flush()
                if demo and not demo.done:
                    outgoing = demo.feed(decoder.decode(chunk))
                    if outgoing:
                        pending = outgoing.encode()
                        deadline = time.monotonic() + (demo.timeout if demo.phase in ("result", "status") else 300)
                        if ci and demo.done:
                            # Let the host's final console output drain before detaching.
                            next_send = time.monotonic() + 1
                            exit_deadline = time.monotonic() + 30
            if sys.stdin in ready:
                data = os.read(0, 4096)
                if not data:
                    inputs.remove(sys.stdin)
                data, exiting = gate.feed(input_filter.feed(data), not ci and (not demo or demo.done))
                if exiting:
                    pending = b""
                    if demo:
                        outgoing = demo.abort("user requested exit")
                        if ci:
                            pending, data = outgoing.encode(), b""
                if data:
                    process.stdin.write(data); process.stdin.flush()
            cleanup_expired = ci and demo.cleanup_deadline is not None and time.monotonic() > demo.cleanup_deadline
            if demo and not demo.done and (time.monotonic() > deadline or cleanup_expired):
                pending = b""
                process.stdin.write(b"\x03"); process.stdin.flush()
                outgoing = demo.abort("timeout waiting for the guest or command result")
                if ci:
                    pending = outgoing.encode()
                    deadline = time.monotonic() + 180
                    if demo.done:
                        exit_deadline = time.monotonic() + 30
            if pending and time.monotonic() >= next_send:
                # Pace bytes through the serial console; wait for a complete
                # shell prompt before issuing the next query.
                process.stdin.write(pending[:16]); process.stdin.flush()
                pending = pending[16:]
                next_send = time.monotonic() + 0.03
            if exit_deadline is not None and time.monotonic() > exit_deadline:
                say("TRIPLE_VM_TEST_FAIL: board launcher did not exit after detaching")
                return 1
        return process.wait()
    finally:
        if saved:
            termios.tcsetattr(0, termios.TCSADRAIN, saved)
            os.write(1, disable_mouse)
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
