#!/usr/bin/env python3
# Copyright 2025 The Axvisor Team
#
# Wrapper for CI: runs `cargo xtask qemu` for NimbOS and automatically sends
# "usertests\n" to the guest when the shell prompt appears, so the test can
# complete without interactive input.
#
# Uses a PTY so the child sees a real TTY; with subprocess.PIPE the child
# may treat stdin as non-interactive and not forward input to QEMU.

import os
import select
import sys
import subprocess

# Trigger strings (try in order; first match sends usertests)
SEND_AFTER = (b"Rust user shell", b">>")
SEND_LINE = b"usertests\n"
SUCCESS_MARKERS = (b"usertests passed!",)


def main():
    try:
        sep = sys.argv.index("--")
    except ValueError:
        print("Usage: ci_run_qemu_nimbos.py -- <command> [args...]", file=sys.stderr)
        sys.exit(2)
    cmd = sys.argv[sep + 1 :]
    if not cmd:
        print("No command after --", file=sys.stderr)
        sys.exit(2)

    import pty

    master, slave = pty.openpty()
    try:
        proc = subprocess.Popen(
            cmd,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            close_fds=True,
        )
    finally:
        os.close(slave)

    sent = False
    saw_success = False
    buffer = b""
    try:
        while True:
            r, _, _ = select.select([master], [], [], 0.1)
            if r:
                try:
                    chunk = os.read(master, 4096)
                except OSError:
                    break
                if not chunk:
                    break
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
                buffer = (buffer + chunk)[-1024:]
                if not saw_success and any(marker in buffer for marker in SUCCESS_MARKERS):
                    saw_success = True
                if not sent and any(trigger in buffer for trigger in SEND_AFTER):
                    try:
                        os.write(master, SEND_LINE)
                        sent = True
                    except OSError:
                        pass
            if proc.poll() is not None:
                while True:
                    r, _, _ = select.select([master], [], [], 0.05)
                    if not r:
                        break
                    try:
                        chunk = os.read(master, 4096)
                    except OSError:
                        break
                    if not chunk:
                        break
                    sys.stdout.buffer.write(chunk)
                    sys.stdout.buffer.flush()
                break
    finally:
        os.close(master)

    if saw_success:
        sys.exit(0)
    sys.exit(proc.returncode if proc.returncode is not None else 1)


if __name__ == "__main__":
    main()
