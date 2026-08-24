#!/usr/bin/env python3
"""Drive the QEMU serial console of an Axvisor run.

Connects to the serial UNIX socket, tees all console output to a log file and
stdout, and executes a small step script:

    sleep <seconds>            wait before the next step
    raw <python-bytes>         write raw bytes (e.g. raw \\x18h for Ctrl+X h)
    cmd <text>                 write text followed by a newline (shell command)
    expect <seconds> <regex>   wait until the regex appears in the output
    send-until <seconds> <interval> <python-bytes> <regex>
                               resend bytes until the regex appears
    attach <vm_id>             switch the attached guest console to <vm_id>
    attach-if-needed <vm_id> <regex>
                               attach unless the regex is already buffered
    detach                     return from the guest console to the shell
    detach-if-attached         return to the shell only when a guest is attached
    dump-pcap <prefix>         stream `virtnet capture dump` and write
                               <prefix>.vm1.pcap / <prefix>.vm2.pcap
    hold <seconds>             keep the connection open and keep reading

Example:
    serial_console.py sock log --script steps.txt
"""

import argparse
import json
import re
import socket
import struct
import sys
import time
from pathlib import Path

DUMP_BEGIN = "CAPDUMP_BEGIN"
DUMP_END = "CAPDUMP_END"
CAPTURE_DUMP_TIMEOUT_SECONDS = 180

PCAP_GLOBAL_HEADER = bytes.fromhex(
    "d4c3b2a1"  # magic, little-endian
    "02000400"  # version 2.4
    "00000000"  # thiszone
    "00000000"  # sigfigs
    "ffff0000"  # snaplen 65535
    "01000000"  # linktype Ethernet
)


class QmpSession:
    def __init__(self, sock_path: str):
        self.conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.conn.settimeout(3)
        self.conn.connect(sock_path)
        self.stream = self.conn.makefile("rwb", buffering=0)
        greeting = self._read_message()
        if "QMP" not in greeting:
            raise RuntimeError(f"invalid QMP greeting: {greeting!r}")
        self.execute("qmp_capabilities")

    def __enter__(self):
        return self

    def __exit__(self, _exc_type, _exc, _traceback):
        self.stream.close()
        self.conn.close()
        return False

    def _read_message(self):
        while True:
            line = self.stream.readline()
            if not line:
                raise RuntimeError("QMP connection closed before a response arrived")
            message = json.loads(line)
            if "event" not in message:
                return message

    def execute(self, command, arguments=None):
        request = {"execute": command}
        if arguments is not None:
            request["arguments"] = arguments
        self.stream.write(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        return self._read_message()


def collect_qmp_forensics(qmp_sock: str, artifact_dir: str) -> None:
    destination = Path(artifact_dir)
    destination.mkdir(parents=True, exist_ok=True)
    requests = [
        ("query-status.json", "query-status", None),
        ("query-cpus-fast.json", "query-cpus-fast", None),
        ("query-chardev.json", "query-chardev", None),
        (
            "info-registers-1.json",
            "human-monitor-command",
            {"command-line": "info registers -a"},
        ),
        (
            "info-registers-2.json",
            "human-monitor-command",
            {"command-line": "info registers -a"},
        ),
    ]
    try:
        with QmpSession(qmp_sock) as qmp:
            for index, (name, command, arguments) in enumerate(requests):
                try:
                    response = qmp.execute(command, arguments)
                except Exception as error:  # Preserve the remaining best-effort snapshots.
                    response = {"driver-error": str(error), "execute": command}
                (destination / name).write_text(
                    json.dumps(response, indent=2, sort_keys=True) + "\n"
                )
                if index == 3:
                    time.sleep(0.5)
    except Exception as error:
        (destination / "qmp-error.txt").write_text(f"{type(error).__name__}: {error}\n")


class ConsoleDriver:
    def __init__(
        self,
        sock_path: str,
        log_path: str,
        timestamp_lines: bool = False,
        progress_regex: str | None = None,
        progress_timeout: float | None = None,
    ):
        deadline = time.time() + 120
        self.conn = None
        while time.time() < deadline:
            try:
                conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                conn.settimeout(0.5)
                conn.connect(sock_path)
                self.conn = conn
                break
            except (FileNotFoundError, ConnectionRefusedError, OSError):
                time.sleep(0.05)
        if self.conn is None:
            raise SystemExit(f"error: serial socket {sock_path} never appeared")
        self.log_file = open(log_path, "a", encoding="utf-8", errors="replace")
        self.timestamp_lines = timestamp_lines
        self.log_pending = ""
        self.progress_pattern = re.compile(progress_regex) if progress_regex else None
        self.progress_timeout = progress_timeout
        self.last_progress = None
        self.progress_tail = ""
        self.watchdog_error = None
        self.tail = b""
        self.dump_lines = []
        self.dumping = False
        self.last_vm = None
        self.attached = False
        self.closed = False

    def write_log(self, text: str) -> None:
        if not self.timestamp_lines:
            self.log_file.write(text)
            self.log_file.flush()
            return
        self.log_pending += text
        while "\n" in self.log_pending:
            line, self.log_pending = self.log_pending.split("\n", 1)
            self.log_file.write(f"[host_monotonic_s={time.monotonic():.6f}] {line}\n")
        self.log_file.flush()

    def observe_progress(self, text: str) -> None:
        if self.progress_pattern is None:
            return
        self.progress_tail = (self.progress_tail + text)[-4096:]
        if self.progress_pattern.search(self.progress_tail):
            self.last_progress = time.monotonic()
            self.progress_tail = ""

    def watchdog_expired(self) -> bool:
        if self.last_progress is None or self.progress_timeout is None:
            return False
        stalled_for = time.monotonic() - self.last_progress
        if stalled_for <= self.progress_timeout:
            return False
        self.watchdog_error = (
            f"no serial progress matching {self.progress_pattern.pattern!r} "
            f"for {stalled_for:.1f} seconds"
        )
        return True

    def close(self) -> None:
        if self.log_pending:
            self.log_file.write(
                f"[host_monotonic_s={time.monotonic():.6f}] {self.log_pending}"
            )
            self.log_pending = ""
        self.conn.close()
        self.log_file.close()

    def poll_reads(self) -> None:
        budget = time.monotonic() + 0.25
        while time.monotonic() < budget:
            try:
                data = self.conn.recv(65536)
            except socket.timeout:
                return
            except OSError:
                self.closed = True
                return
            if not data:
                self.closed = True
                return
            text = data.decode("utf-8", errors="replace")
            sys.stdout.write(text)
            sys.stdout.flush()
            self.write_log(text)
            self.observe_progress(text)
            if self.dumping:
                self.dump_lines.append(text)
            self.tail = (self.tail + data)[-1_000_000:]
            console_tail = self.tail[-512:].decode("utf-8", errors="replace")
            for match in re.finditer(
                r"\[Axvisor\] (attached|detached) VM\[(\d+)\]", console_tail
            ):
                self.attached = match.group(1) == "attached"
                self.last_vm = int(match.group(2))

    def wait_for(self, pattern: str, seconds: float) -> bool:
        end = time.time() + seconds
        while time.time() < end and not self.closed:
            self.poll_reads()
            if self.watchdog_expired():
                return False
            if re.search(pattern, self.tail.decode("utf-8", errors="replace")):
                return True
            time.sleep(0.3)
        return False

    def hold(self, seconds: float) -> None:
        end = time.time() + seconds
        while time.time() < end and not self.closed:
            self.poll_reads()
            if self.watchdog_expired():
                return
            time.sleep(0.3)

    def hold_ignoring_watchdog(self, seconds: float) -> None:
        end = time.monotonic() + seconds
        while time.monotonic() < end and not self.closed:
            self.poll_reads()
            time.sleep(0.1)

    def send_until(
        self, payload: bytes, pattern: str, seconds: float, interval: float
    ) -> bool:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline and not self.closed:
            self.conn.sendall(payload)
            remaining = deadline - time.monotonic()
            if self.wait_for(pattern, min(interval, max(0.0, remaining))):
                return True
            if self.watchdog_error:
                return False
        return False

    def attach(self, vm_id: int) -> None:
        for _ in range(4):
            if self.attached and self.last_vm == vm_id:
                return
            self.conn.sendall(b"\x18]")
            deadline = time.time() + 2
            while time.time() < deadline:
                self.poll_reads()
                if self.attached and self.last_vm is not None:
                    break
        if not (self.attached and self.last_vm == vm_id):
            print(
                f"warning: could not confirm attachment to VM {vm_id}",
                file=sys.stderr,
            )

    def attach_if_needed(self, vm_id: int, pattern: str) -> None:
        if re.search(pattern, self.tail.decode("utf-8", errors="replace")):
            return
        self.attach(vm_id)

    def dump_pcap(self, prefix: str) -> None:
        """Stream the in-hypervisor capture and write one pcap per Guest."""
        self.dump_lines = []
        self.dumping = True
        self.conn.sendall(b"virtnet capture dump\n")
        deadline = time.time() + CAPTURE_DUMP_TIMEOUT_SECONDS
        got_end = False
        while time.time() < deadline and not self.closed:
            self.poll_reads()
            joined = "".join(self.dump_lines)
            if DUMP_END in joined:
                got_end = True
                break
            time.sleep(0.3)
        self.dumping = False
        if not got_end:
            raise RuntimeError("capture dump did not complete")

        joined = "".join(self.dump_lines)
        begin = joined.find(DUMP_BEGIN)
        end = joined.find(DUMP_END)
        if begin < 0 or end <= begin:
            raise RuntimeError("capture dump markers are malformed")
        body = joined[begin + len(DUMP_BEGIN):end]
        frames = {1: [], 2: []}
        for line in body.splitlines():
            match = re.match(r"CAPTURE (\d+) (\d+) ([0-9a-f]+)", line.strip())
            if not match:
                continue
            vm = int(match.group(1))
            nanos = int(match.group(2))
            frame = bytes.fromhex(match.group(3))
            frames.setdefault(vm, []).append((nanos, frame))

        prefix_path = Path(prefix)
        prefix_path.parent.mkdir(parents=True, exist_ok=True)
        for vm, records in sorted(frames.items()):
            pcap_path = prefix_path.with_name(f"{prefix_path.name}.vm{vm}.pcap")
            with pcap_path.open("wb") as pcap_file:
                pcap_file.write(PCAP_GLOBAL_HEADER)
                for nanos, frame in records:
                    seconds = nanos // 1_000_000_000
                    micros = (nanos // 1_000) % 1_000_000
                    length = len(frame)
                    pcap_file.write(
                        struct.pack("<IIII", seconds, micros, length, length)
                    )
                    pcap_file.write(frame)
            print(f"pcap: wrote {len(records)} frames to {pcap_path}")
        self.dump_lines = []

    def collect_forensics(self, qmp_sock: str | None, artifact_dir: str | None) -> None:
        if not artifact_dir:
            return
        destination = Path(artifact_dir)
        destination.mkdir(parents=True, exist_ok=True)
        self.write_log("\n[driver] progress watchdog fired; collecting post-stall forensics\n")

        if qmp_sock:
            collect_qmp_forensics(qmp_sock, artifact_dir)
        else:
            (destination / "qmp-error.txt").write_text("QMP socket was not configured\n")

        actions = [
            (b"\x18h", "detach to Axvisor shell"),
            (b"rt stat\n", "RT snapshot 1"),
            (b"vmexit stat\n", "VM-exit snapshot 1"),
            (b"rt stat\n", "RT snapshot 2"),
            (b"vmexit stat\n", "VM-exit snapshot 2"),
            (b"vm console 1\n", "reattach Linux VM"),
        ]
        action_log = []
        for payload, description in actions:
            try:
                self.conn.sendall(payload)
                action_log.append(f"sent: {description}")
            except OSError as error:
                action_log.append(f"failed: {description}: {error}")
            self.hold_ignoring_watchdog(1.0)
        (destination / "serial-actions.txt").write_text("\n".join(action_log) + "\n")
        (destination / "serial-tail.bin").write_bytes(self.tail)
        self.dumping = True
        try:
            self.conn.sendall(b"virtnet capture dump\n")
        except OSError as error:
            self.dumping = False
            print(f"error: capture dump unavailable: {error}", file=sys.stderr)
            return
        deadline = time.time() + CAPTURE_DUMP_TIMEOUT_SECONDS
        got_end = False
        while time.time() < deadline and not self.closed:
            self.poll_reads()
            joined = "".join(self.dump_lines)
            if DUMP_END in joined:
                got_end = True
                break
            time.sleep(0.3)
        self.dumping = False
        if not got_end:
            print("error: capture dump did not complete", file=sys.stderr)
            return
        joined = "".join(self.dump_lines)
        begin = joined.find(DUMP_BEGIN)
        end = joined.find(DUMP_END)
        body = joined[begin + len(DUMP_BEGIN):end]
        frames = {1: [], 2: []}
        for line in body.splitlines():
            match = re.match(r"CAPTURE (\d+) (\d+) ([0-9a-f]+)", line.strip())
            if not match:
                continue
            vm = int(match.group(1))
            nanos = int(match.group(2))
            frame = bytes.fromhex(match.group(3))
            frames.setdefault(vm, []).append((nanos, frame))
        prefix = destination / "capture"
        for vm, records in sorted(frames.items()):
            pcap_path = prefix.with_name(f"{prefix.name}.vm{vm}.pcap")
            with pcap_path.open("wb") as pcap_file:
                pcap_file.write(PCAP_GLOBAL_HEADER)
                for nanos, frame in records:
                    seconds = nanos // 1_000_000_000
                    micros = (nanos // 1_000) % 1_000_000
                    length = len(frame)
                    pcap_file.write(
                        struct.pack("<IIII", seconds, micros, length, length)
                    )
                    pcap_file.write(frame)
            print(f"pcap: wrote {len(records)} frames to {pcap_path}")
        self.dump_lines = []


    def qmp_quit(self, qmp_sock: str) -> None:
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                qmp = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                qmp.settimeout(3)
                qmp.connect(qmp_sock)
                qmp.recv(4096)
                qmp.sendall(b'{"execute":"qmp_capabilities"}\n')
                time.sleep(0.5)
                qmp.recv(4096)
                qmp.sendall(b'{"execute":"quit"}\n')
                time.sleep(1)
                qmp.close()
                return
            except (FileNotFoundError, ConnectionRefusedError, OSError):
                time.sleep(2)
        print("warning: could not quit QEMU over QMP", file=sys.stderr)


def report_watchdog_failure(driver: ConsoleDriver, args) -> int:
    print(f"error: {driver.watchdog_error}", file=sys.stderr)
    driver.collect_forensics(args.qmp_sock, args.forensics_dir)
    return 4


def report_expectation_failure(driver: ConsoleDriver, args, message: str) -> int:
    print(f"error: {message}", file=sys.stderr)
    driver.collect_forensics(args.qmp_sock, args.forensics_dir)
    return 2


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("sock", help="serial UNIX socket path")
    parser.add_argument("log", help="console log file to append to")
    parser.add_argument("--script", help="step script file")
    parser.add_argument("--verbose", action="store_true", help="log step progress to stderr")
    parser.add_argument(
        "--timestamp-lines",
        action="store_true",
        help="prefix persisted serial lines with the host monotonic timestamp",
    )
    parser.add_argument("--progress-regex", help="serial regex that resets the progress watchdog")
    parser.add_argument(
        "--progress-timeout",
        type=float,
        help="fail after this many host seconds without another progress marker",
    )
    parser.add_argument("--qmp-sock", help="QMP socket used for post-stall snapshots")
    parser.add_argument(
        "--forensics-dir",
        help="directory for best-effort serial and QMP post-stall artifacts",
    )
    args = parser.parse_args()

    driver = ConsoleDriver(
        args.sock,
        args.log,
        timestamp_lines=args.timestamp_lines,
        progress_regex=args.progress_regex,
        progress_timeout=args.progress_timeout,
    )

    steps = []
    if args.script:
        with open(args.script, encoding="utf-8") as script_file:
            for line in script_file:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                steps.append(line)

    for step in steps:
        if driver.closed:
            break
        if args.verbose:
            print(f"[driver] step: {step!r}", file=sys.stderr, flush=True)
        if step.startswith("sleep "):
            driver.hold(float(step.split(" ", 1)[1]))
        elif step.startswith("hold "):
            driver.hold(float(step.split(" ", 1)[1]))
        elif step.startswith("raw "):
            payload = step.split(" ", 1)[1]
            encoded = payload.encode().decode("unicode_escape").encode("latin-1")
            driver.conn.sendall(encoded)
            time.sleep(0.3)
        elif step.startswith("cmd "):
            driver.conn.sendall((step.split(" ", 1)[1] + "\n").encode())
            time.sleep(0.3)
        elif step.startswith("expect "):
            _, seconds, pattern = step.split(" ", 2)
            if not driver.wait_for(pattern, float(seconds)):
                if driver.watchdog_error:
                    status = report_watchdog_failure(driver, args)
                    driver.close()
                    return status
                status = report_expectation_failure(
                    driver,
                    args,
                    f"expected pattern {pattern!r} did not appear",
                )
                driver.close()
                return status
        elif step.startswith("send-until "):
            _, seconds, interval, payload, pattern = step.split(" ", 4)
            encoded = payload.encode().decode("unicode_escape").encode("latin-1")
            if not driver.send_until(
                encoded, pattern, float(seconds), float(interval)
            ):
                if driver.watchdog_error:
                    status = report_watchdog_failure(driver, args)
                    driver.close()
                    return status
                status = report_expectation_failure(
                    driver,
                    args,
                    f"pattern {pattern!r} did not appear while resending input",
                )
                driver.close()
                return status
        elif step.startswith("attach "):
            driver.attach(int(step.split(" ", 1)[1]))
        elif step.startswith("attach-if-needed "):
            _, vm_id, pattern = step.split(" ", 2)
            driver.attach_if_needed(int(vm_id), pattern)
        elif step == "detach":
            driver.conn.sendall(b"\x18h")
            time.sleep(0.3)
        elif step == "detach-if-attached":
            if driver.attached:
                driver.conn.sendall(b"\x18h")
                time.sleep(0.3)
        elif step == "clear-tail":
            driver.tail = b""
            driver.progress_tail = ""
        elif step.startswith("dump-pcap "):
            driver.dump_pcap(step.split(" ", 1)[1])
        elif step.startswith("qmp-quit "):
            driver.qmp_quit(step.split(" ", 1)[1])
        else:
            print(f"error: unknown step {step!r}", file=sys.stderr)
            return 3

    driver.hold(2)
    driver.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
