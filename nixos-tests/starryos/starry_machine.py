#!/usr/bin/env python3

"""Serial evaluator and fail-closed command-channel proxy for Starry nixosTest."""

from __future__ import annotations

import re
from typing import Any

PHASE_ARTIFACT_PREPARATION = "artifact-preparation"
PHASE_MACHINE_STARTUP = "machine-startup"
PHASE_STAGE2_ACTIVATION = "stage2-activation"
PHASE_GUEST_ASSERTION = "guest-assertion"
PHASE_UNEXPECTED_GUEST_EXIT = "unexpected-guest-exit"
PHASE_SHUTDOWN = "shutdown"
PHASE_TIMEOUT = "timeout"

BOOT_SUCCESS_PATTERN = re.compile(
    r"(?s:STARRY_NIXOS_PHASE=pid1.*"
    r"STARRY_NIXOS_PHASE=activation.*"
    r"STARRY_NIXOS_PHASE=systemd.*"
    r"STARRY_NIXOS_PHASE=marker.*"
    r"STARRY_NIXOS_SYSTEM_PASSED)"
)
BOOT_FAILURE_PATTERNS = [
    re.compile(r"(?i:\bpanic(?:ked)?\b)"),
    re.compile(r"(?i:\bfatal\b)"),
    re.compile(r"(?m:^.*(?:starry-nixos-)?marker\.service: Failed with result)"),
    re.compile(r"(?m:^Failed to start Verify the StarryNixOS stage-2 baseline\.?$)"),
    re.compile(r"(?m:^STARRY_NIXOS_SYSTEM_FAILED:)"),
]
ASSERT_PASSED = "STARRY_NIXOS_ASSERT_PASSED"
ASSERT_FAILED_PREFIX = "STARRY_NIXOS_ASSERT_FAILED:"
ASSERT_BEGIN = "STARRY_NIXOS_ASSERT_BEGIN"
ASSERT_CMD_PREFIX = "STARRY_NIXOS_ASSERT_CMD="
ASSERT_STATUS_PREFIX = "STARRY_NIXOS_ASSERT_STATUS="
ASSERT_OUTPUT_BEGIN = "STARRY_NIXOS_ASSERT_OUTPUT_BEGIN"
ASSERT_OUTPUT_END = "STARRY_NIXOS_ASSERT_OUTPUT_END"
UNSUPPORTED_PREFIX = "unsupported Starry nixosTest operation:"
PHASE_FAILED_PREFIX = "STARRY_NIXOS_PHASE_FAILED="

FORBIDDEN_METHODS = frozenset(
    {
        "succeed",
        "fail",
        "execute",
        "wait_for_unit",
        "wait_until_succeeds",
        "systemctl",
        "shutdown",
        "copy_from_host",
        "copy_from_host_via_shell",
        "copy_from_machine",
        "shell_interact",
    }
)

CONSOLE_TAIL_LINES = 40


class StarryNixosTestError(Exception):
    """Named failure with an FR-011 phase and retained serial evidence."""


def last_console_lines(console: str, limit: int = CONSOLE_TAIL_LINES) -> str:
    lines = console.splitlines()
    return "\n".join(lines[-limit:])


def phase_failed_message(phase: str, reason: str, console: str = "") -> str:
    evidence = last_console_lines(console)
    parts = [f"{PHASE_FAILED_PREFIX}{phase}", reason]
    if evidence:
        parts.append("last serial evidence:")
        parts.append(evidence)
    return "\n".join(parts)


def raise_phase(phase: str, reason: str, console: str = "") -> None:
    raise StarryNixosTestError(phase_failed_message(phase, reason, console))


def parse_assertion_record(console: str) -> dict[str, Any]:
    failed_index = console.find(ASSERT_FAILED_PREFIX)
    if failed_index >= 0:
        line = console[failed_index:].splitlines()[0]
        reason = line[len(ASSERT_FAILED_PREFIX) :].strip()
        return {
            "result": "Failed",
            "reason": reason,
            "command": None,
            "status": None,
            "output": None,
        }

    if ASSERT_BEGIN not in console or ASSERT_PASSED not in console:
        raise_phase(
            PHASE_GUEST_ASSERTION,
            "missing STARRY_NIXOS_ASSERT_BEGIN/PASSED block",
            console,
        )

    def required_field(prefix: str) -> str:
        match = re.search(rf"(?m)^{re.escape(prefix)}(.*)$", console)
        if match is None:
            raise_phase(
                PHASE_GUEST_ASSERTION,
                f"missing {prefix.rstrip('=')} record",
                console,
            )
        return match.group(1)

    command = required_field(ASSERT_CMD_PREFIX)
    status_text = required_field(ASSERT_STATUS_PREFIX)
    try:
        status = int(status_text)
    except ValueError:
        raise_phase(
            PHASE_GUEST_ASSERTION,
            f"invalid STARRY_NIXOS_ASSERT_STATUS={status_text!r}",
            console,
        )

    begin = console.find(ASSERT_OUTPUT_BEGIN)
    end = console.find(ASSERT_OUTPUT_END)
    if begin < 0 or end < 0 or end < begin:
        raise_phase(
            PHASE_GUEST_ASSERTION,
            "missing STARRY_NIXOS_ASSERT_OUTPUT delimiters",
            console,
        )
    output_block = console[begin + len(ASSERT_OUTPUT_BEGIN) : end]
    output = output_block.strip("\n")
    return {
        "result": "Passed",
        "reason": None,
        "command": command,
        "status": status,
        "output": output,
    }


def classify_boot_failure(console: str, *, terminal_seen: bool, qemu_exited: bool) -> str:
    if any(pattern.search(console) for pattern in BOOT_FAILURE_PATTERNS):
        if "STARRY_NIXOS_PHASE=" in console:
            return PHASE_UNEXPECTED_GUEST_EXIT
        return PHASE_UNEXPECTED_GUEST_EXIT
    if not terminal_seen:
        if qemu_exited and "STARRY_NIXOS_PHASE=" not in console:
            return PHASE_MACHINE_STARTUP
        if "STARRY_NIXOS_PHASE=" in console:
            return PHASE_STAGE2_ACTIVATION
        return PHASE_TIMEOUT
    if not BOOT_SUCCESS_PATTERN.search(console):
        return PHASE_STAGE2_ACTIVATION
    return PHASE_STAGE2_ACTIVATION


def evaluate_boot_console(
    console: str,
    *,
    terminal_seen: bool,
    qemu_exited: bool,
) -> None:
    if any(pattern.search(console) for pattern in BOOT_FAILURE_PATTERNS):
        raise_phase(
            PHASE_UNEXPECTED_GUEST_EXIT,
            "console matched a terminal failure pattern",
            console,
        )
    if not terminal_seen:
        phase = classify_boot_failure(
            console, terminal_seen=False, qemu_exited=qemu_exited
        )
        if phase == PHASE_TIMEOUT:
            raise_phase(
                PHASE_TIMEOUT,
                "StarryNixOS boot produced no terminal evidence within 600 seconds",
                console,
            )
        if phase == PHASE_MACHINE_STARTUP:
            raise_phase(
                PHASE_MACHINE_STARTUP,
                "QEMU exited before any StarryNixOS stage-2 phase marker",
                console,
            )
        raise_phase(
            PHASE_STAGE2_ACTIVATION,
            "StarryNixOS boot did not reach the ordered success contract",
            console,
        )
    if not BOOT_SUCCESS_PATTERN.search(console):
        raise_phase(
            PHASE_STAGE2_ACTIVATION,
            "StarryNixOS boot did not reach the ordered success contract",
            console,
        )


def evaluate_service_assertion(
    console: str,
    *,
    expected_status: int = 0,
    expected_output: str | None = None,
    require_pass: bool = True,
) -> dict[str, Any]:
    record = parse_assertion_record(console)
    if require_pass:
        if record["result"] != "Passed":
            raise_phase(
                PHASE_GUEST_ASSERTION,
                f"assertion failed: {record.get('reason') or 'ASSERT_FAILED'}",
                console,
            )
        if record["status"] != expected_status:
            raise_phase(
                PHASE_GUEST_ASSERTION,
                f"assertion status {record['status']} != {expected_status}",
                console,
            )
        if expected_output is not None and expected_output not in (record["output"] or ""):
            raise_phase(
                PHASE_GUEST_ASSERTION,
                f"assertion output missing expected {expected_output!r}",
                console,
            )
        return record

    if record["result"] == "Passed" and record["status"] == 0:
        raise_phase(
            PHASE_GUEST_ASSERTION,
            "negative service case unexpectedly passed",
            console,
        )
    return record


class StarryMachine:
    """Fail-closed proxy around an upstream nixosTest machine."""

    def __init__(self, inner: Any):
        object.__setattr__(self, "_inner", inner)

    def __getattr__(self, name: str) -> Any:
        if name in FORBIDDEN_METHODS:
            raise StarryNixosTestError(
                phase_failed_message(
                    PHASE_GUEST_ASSERTION,
                    f"{UNSUPPORTED_PREFIX} {name}",
                )
            )
        return getattr(self._inner, name)

    def __setattr__(self, name: str, value: Any) -> None:
        if name == "_inner":
            object.__setattr__(self, name, value)
            return
        setattr(self._inner, name, value)


def wrap_machine(machine: Any) -> StarryMachine:
    return StarryMachine(machine)
