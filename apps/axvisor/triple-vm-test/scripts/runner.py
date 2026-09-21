#!/usr/bin/env python3
"""Boot and check three concurrent guests as one independent acceptance case."""
import argparse
import json
from pathlib import Path
import re
import sys
import tempfile
import time

from console import run_console, say
from linux_checks import LinuxChecks
from shell_checks import ShellChecks, arceos_result, zephyr_result

CASE = Path(__file__).resolve().parents[1]
REPO = CASE.parents[2]
HOST_PROMPT = re.compile(r"axvisor:/*\$\s*\Z")
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
EXPECTED = {1: "linux-triple", 2: "arceos-triple", 3: "zephyr-triple"}


class Demo:
    def __init__(self, emit, linux_prompt, ping_target, timeout, proxy, ci=False):
        self.say = emit
        self.ci = ci
        self.checks = [
            (2, ShellChecks("ArceOS", "arceos:/$", ["help", "uname"], arceos_result, emit)),
            (3, ShellChecks("Zephyr", "zephyr:~$",
                            ["kernel version", "kernel thread list", "device list"],
                            zephyr_result, emit)),
            (1, LinuxChecks(linux_prompt, emit, ping_target, timeout, proxy)),
        ]
        self.stage, self.buffer, self.index = "host", "", 0
        self.done, self.passed = False, False

    @property
    def phase(self):
        return self.checks[self.index][1].phase if self.stage == "guest" else self.stage

    @property
    def timeout(self):
        return self.checks[self.index][1].timeout

    def abort(self, reason):
        if not self.done:
            self.done = True
            self.say(f"\nTRIPLE_VM_TEST_FAIL: {reason}")
            if not self.ci:
                self.say("Keyboard input restored. Inspect active guest commands before shutdown.")

    def list_is_running(self, text):
        rows = re.findall(r"(?m)^\s*(\d+)\s+(\S+)\s+(\S+)\s+", text)
        actual = {int(vm_id): (name, state.lower()) for vm_id, name, state in rows}
        return set(actual) == set(EXPECTED) and all(
            actual[vm_id] == (name, "running") for vm_id, name in EXPECTED.items())

    def feed(self, text):
        if self.done:
            return None
        if self.stage == "guest":
            checker = self.checks[self.index][1]
            command = checker.feed(text)
            if checker.done:
                if not checker.passed:
                    self.abort(f"VM {self.checks[self.index][0]} checks failed")
                    return None
                self.stage, self.buffer = "return", ""
                return "\x18h"
            return command

        self.buffer = (self.buffer + text)[-65536:]
        clean = ANSI.sub("", self.buffer).replace("\r", "")
        if self.stage == "host":
            if HOST_PROMPT.search(clean):
                self.stage, self.buffer = "initial-list", ""
                return "vm list\n"
        elif self.stage in ("initial-list", "final-list"):
            if "VM ID" not in clean or not HOST_PROMPT.search(clean):
                return None
            if not self.list_is_running(clean):
                self.abort("expected Linux, ArceOS and Zephyr all running")
                return None
            if self.stage == "final-list":
                self.done, self.passed = True, True
                if not self.ci:
                    self.say("\nTRIPLE_VM_TEST_PASS")
                    self.say("Three guests remain running. SSH is checked separately as documented.")
                return None
            self.stage, self.buffer = "attach", ""
            return f"vm console {self.checks[self.index][0]}\n"
        elif self.stage == "attach":
            if f"Attached VM[{self.checks[self.index][0]}] console;" in clean:
                self.stage, self.buffer = "guest", ""
                return "\n"
        elif self.stage == "return" and HOST_PROMPT.search(clean):
            self.buffer = ""
            if self.index + 1 == len(self.checks):
                self.stage = "final-list"
                return "vm list\n"
            self.index += 1
            self.stage = "attach"
            return f"vm console {self.checks[self.index][0]}\n"
        return None


class CiRun:
    """Keep the lease until guest shutdown; fail closed on incomplete cleanup."""

    ci = True

    def __init__(self, demo):
        self.demo = demo
        self.stage, self.buffer = "checks", ""
        self.done, self.passed = False, False
        self.cleanup_deadline = None
        self.password_sent = False

    @property
    def phase(self):
        return self.demo.phase if self.stage == "checks" else self.stage

    @property
    def timeout(self):
        return self.demo.timeout

    def transition(self, stage, command):
        self.stage, self.buffer = stage, ""
        return command

    @staticmethod
    def vm_states(text):
        rows = re.findall(r"(?m)^\s*(\d+)\s+(\S+)\s+(\S+)\s+", text)
        actual = {int(vm_id): (name, state.lower()) for vm_id, name, state in rows}
        if set(actual) != set(EXPECTED) or any(
            actual[vm_id][0] != name for vm_id, name in EXPECTED.items()
        ):
            return None
        return {vm_id: state for vm_id, (_, state) in actual.items()}

    def start_cleanup(self):
        self.cleanup_deadline = time.monotonic() + 180
        self.demo.say("\nCI: shutting down guests before releasing the board.")
        return self.transition("host", "\x18h\n")

    def stop_remaining(self, states):
        for vm_id, stage in ((2, "stop-arceos"), (3, "stop-zephyr")):
            if states[vm_id] == "running":
                return self.transition(stage, f"vm stop {vm_id}\n")
            if states[vm_id] not in ("stopped", "stopping"):
                return self.abort(f"VM {vm_id} is not in a state allowing cleanup")
        if any(state == "stopping" for state in states.values()):
            return self.transition("stopped", "vm list\n")
        if all(state == "stopped" for state in states.values()):
            return self.transition("exit", "exit\n")
        return self.abort("cannot complete guest shutdown")

    def abort(self, reason):
        self.demo.passed = False
        if self.stage == "checks":
            self.demo.abort(reason)
            return self.start_cleanup()
        self.demo.say(f"\nTRIPLE_VM_TEST_FAIL: cleanup incomplete: {reason}")
        self.done, self.passed = True, False
        return "\x01x"

    def feed(self, text):
        if self.stage == "checks":
            command = self.demo.feed(text)
            return self.start_cleanup() if self.demo.done else command
        self.buffer = (self.buffer + text)[-65536:]
        clean = ANSI.sub("", self.buffer).replace("\r", "")
        prompt = HOST_PROMPT.search(clean)
        if self.stage == "host" and prompt:
            return self.transition("linux-state", "vm list\n")
        if self.stage in ("linux-state", "stopped") and "VM ID" in clean and prompt:
            states = self.vm_states(clean)
            if states is None:
                return self.abort("cannot confirm all three VM states during cleanup")
            if self.stage == "stopped":
                return self.stop_remaining(states)
            if states.get(1) == "running":
                return self.transition("attach", "vm console 1\n")
            if states.get(1) == "stopped":
                return self.stop_remaining(states)
            if states.get(1) == "stopping":
                return self.transition("linux-state", "vm list\n")
            return self.abort("Linux is not in a state allowing orderly shutdown")
        if self.stage == "attach" and "Attached VM[1] console;" in clean:
            return self.transition("linux-prompt", "\x03\n")
        if self.stage == "linux-prompt" and self.demo.checks[-1][1].prompt_match(clean):
            return self.transition("poweroff", "sync && sudo -p 'Password: ' -- poweroff\n")
        if self.stage == "poweroff":
            if not self.password_sent and re.search(r"(?:^|\n)Password: $", clean):
                self.password_sent, self.buffer = True, ""
                return "orangepi\n"
            if "VM[1] stopped; returning to the management shell" in clean and prompt:
                return self.transition("linux-state", "vm list\n")
        if self.stage == "stop-arceos" and "VM[2] stop signal sent successfully" in clean and prompt:
            return self.transition("stopped", "vm list\n")
        if self.stage == "stop-zephyr" and "VM[3] stop signal sent successfully" in clean and prompt:
            return self.transition("stopped", "vm list\n")
        if self.stage == "exit" and re.search(r"(?m)^Goodbye!$", clean):
            self.done, self.passed = True, self.demo.passed
            return "\x01x"
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--interactive", action="store_true", help="boot only; run no checks")
    mode.add_argument("--ci", action="store_true", help="check, shut down guests and release the board")
    parser.add_argument("--server")
    parser.add_argument("--port", type=int)
    parser.add_argument("--board-type", required=True)
    parser.add_argument("--linux-prompt", default="orangepi@orangepi5plus:~")
    parser.add_argument("--ping-target")
    parser.add_argument("--apt-proxy", help="HTTP proxy used only for guest APT commands")
    parser.add_argument("--timeout", type=int, default=600, help="Linux command timeout in seconds")
    args = parser.parse_args()
    if args.timeout <= 0 or (args.port is not None and not 1 <= args.port <= 65535):
        parser.error("timeout and port must be positive and valid")
    for value in (args.board_type, args.linux_prompt, args.ping_target, args.apt_proxy):
        if value is not None and (not value.strip() or any(ord(c) < 32 for c in value)):
            parser.error("board type, prompt, target and proxy must be nonempty without control characters")
    if args.ping_target and args.ping_target.startswith("-"):
        parser.error("ping target cannot start with '-'")
    demo = None if args.interactive else Demo(
        say, args.linux_prompt, args.ping_target, args.timeout, args.apt_proxy, args.ci)
    if args.ci:
        demo = CiRun(demo)
    say("Manual interactive mode." if demo is None else
        "Three-guest checks: ArceOS shell, Zephyr shell, Linux CPU/network/files/APT.")
    try:
        with tempfile.TemporaryDirectory(prefix="axvisor-triple-vm-") as temp_dir:
            board_config = Path(temp_dir) / "board.toml"
            # axbuild loads a BoardRunConfig before applying command-line overrides.
            board_config.write_text(
                f"board_type = {json.dumps(args.board_type)}\n",
                encoding="utf-8",
            )
            command = ["cargo", "xtask", "axvisor", "board", "--config",
                       "apps/axvisor/triple-vm-test/configs/build.toml", "--board-config",
                       str(board_config), "--board-type", args.board_type]
            for flag, value in (("--server", args.server), ("--port", args.port)):
                if value is not None:
                    command.extend((flag, str(value)))
            status = run_console(command, demo, REPO)
    except (OSError, KeyboardInterrupt) as error:
        say(f"Run interrupted: {error}")
        return 1
    if demo and not demo.done:
        demo.abort("terminal closed before all checks completed")
    if args.ci:
        say("TRIPLE_VM_TEST_PASS" if status == 0 and demo.passed else "TRIPLE_VM_TEST_FAIL")
    return status if status else int(demo is not None and not demo.passed)


if __name__ == "__main__":
    sys.exit(main())
