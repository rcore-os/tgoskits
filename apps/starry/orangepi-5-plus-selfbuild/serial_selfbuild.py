#!/usr/bin/env python3
"""Drive one StarryOS self-build after Linux has selected its boot script."""

from __future__ import annotations

import argparse
import os
import pathlib
import re
import sys
import time

import serial


MARKER = "STARRY-ORANGEPI5PLUS-SELFBUILD"
SHELL_PROMPT = b"root@starry:/root #"
SHELL_PROMPT_PATTERN = re.compile(rb"root@starry:(?:/root #|~# )")
GUEST_COMMAND = b"sh /opt/starry-orangepi5plus-selfbuild/init.sh\r"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--serial", required=True, dest="serial_device")
    parser.add_argument("--log", required=True, type=pathlib.Path)
    parser.add_argument("--ready-file", required=True, type=pathlib.Path)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--kernel-only", action="store_true")
    parser.add_argument("--timeout", type=int, default=22_800)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9._-]+", args.run_id):
        parser.error("--run-id contains unsupported characters")
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    return args


def append_output(log_file, data: bytes) -> None:
    log_file.write(data)
    log_file.flush()
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()


def main() -> int:
    args = parse_args()
    guest_command = GUEST_COMMAND
    if args.kernel_only:
        guest_command = (
            "sh /opt/starry-orangepi5plus-selfbuild/init-kernel-selfbuild.sh "
            f"{args.run_id}\r"
        ).encode()
    args.log.parent.mkdir(parents=True, exist_ok=True)
    args.ready_file.parent.mkdir(parents=True, exist_ok=True)
    deadline = time.monotonic() + args.timeout
    success = f"==={MARKER}-PASS run={args.run_id} ".encode()
    failures = (
        f"==={MARKER}-FAIL".encode(),
        f"==={MARKER}-TIMEOUT===".encode(),
    )
    window = b""
    prompt_carry = b""
    prompt_generation = 0

    with args.log.open("ab", buffering=0) as log_file:
        with serial.Serial(
            args.serial_device,
            baudrate=1_500_000,
            timeout=0.25,
            write_timeout=2,
            exclusive=True,
        ) as uart:
            args.ready_file.write_text(
                f"pid={os.getpid()}\nserial={args.serial_device}\n",
                encoding="utf-8",
            )
            print(f"serial_selfbuild_ready={args.serial_device}", flush=True)
            while time.monotonic() < deadline:
                data = uart.read(uart.in_waiting or 1)
                if not data:
                    continue
                append_output(log_file, data)
                window = (window + data)[-131_072:]

                if success in window:
                    print(f"serial_selfbuild_pass={args.run_id}", flush=True)
                    return 0
                if any(marker in window for marker in failures):
                    print(f"serial_selfbuild_fail={args.run_id}", flush=True)
                    return 1

                prompt_input = prompt_carry + data
                prompts = list(SHELL_PROMPT_PATTERN.finditer(prompt_input))
                unconsumed = prompt_input[prompts[-1].end() :] if prompts else prompt_input
                prompt_carry = unconsumed[-(len(SHELL_PROMPT) - 1) :]
                for _ in prompts:
                    if args.kernel_only and prompt_generation:
                        print(
                            f"serial_selfbuild_incomplete={args.run_id}",
                            flush=True,
                        )
                        return 1
                    # Re-inject after an unexpected StarryOS reboot as well.
                    # init.sh restores the Linux selector before doing any work.
                    uart.write(guest_command)
                    uart.flush()
                    prompt_generation += 1
                    print(
                        f"serial_selfbuild_command_sent={prompt_generation}",
                        flush=True,
                    )

    print(f"serial_selfbuild_timeout={args.run_id}", file=sys.stderr)
    return 124


if __name__ == "__main__":
    raise SystemExit(main())
