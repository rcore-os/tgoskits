import importlib.util
from pathlib import Path

import pytest


RUNNER_PATH = (
    Path(__file__).resolve().parents[2] / "board/run-atk-task23-blackout.py"
)
SPEC = importlib.util.spec_from_file_location("run_atk_task23_blackout", RUNNER_PATH)
assert SPEC is not None and SPEC.loader is not None
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)
RUNNER_SOURCE = RUNNER_PATH.read_text()


def complete_log() -> bytes:
    return b"\n".join(
        (
            b"STARRY_T2N1_STATUS_DELIVERED request=1",
            b"virtnet: blackout ON",
            b"STARRY_T2N1_SAFE source=protocol",
            b"TASK2_SAFE state=Safe event=HeartbeatTimeout",
            b"virtnet: blackout OFF",
            b"STARRY_T2N1_RECOVERED state=Active",
            b"TASK2_CONTROL_RECEIVED request=2",
            b"TASK2_STATUS_SENT seq=1",
            b"virtnet switch: blackout=off",
            b"atk-task123-starry running",
            b"atk-task123-zephyr running",
        )
    )


def test_validator_accepts_complete_ordered_recovery_evidence() -> None:
    RUNNER.validate_evidence(complete_log())


def test_runner_uses_the_physical_axvisor_console_command() -> None:
    assert 'f"vm console {vm_id}"' in RUNNER_SOURCE
    assert 'self.command(f"vm attach {vm_id}"' not in RUNNER_SOURCE
    assert r"Attached VM\[[12]\] console" in RUNNER_SOURCE


def test_validator_rejects_missing_zephyr_recovery_control() -> None:
    with pytest.raises(RuntimeError, match="TASK2_CONTROL_RECEIVED"):
        RUNNER.validate_evidence(
            complete_log().replace(b"TASK2_CONTROL_RECEIVED request=2\n", b"")
        )


def test_validator_rejects_recovery_before_blackout_is_lifted() -> None:
    log = complete_log().replace(
        b"virtnet: blackout OFF\nSTARRY_T2N1_RECOVERED state=Active",
        b"STARRY_T2N1_RECOVERED state=Active\nvirtnet: blackout OFF",
    )
    with pytest.raises(RuntimeError, match="missing evidence marker"):
        RUNNER.validate_evidence(log)


class RecordingConsole:
    def __init__(self) -> None:
        self.calls: list[tuple] = []

    def detach(self) -> None:
        self.calls.append(("detach",))

    def command(self, command: str, prompt: bytes) -> None:
        self.calls.append(("command", command, prompt))

    def attach(self, vm_id: int) -> None:
        self.calls.append(("attach", vm_id))

    def clear(self) -> None:
        self.calls.append(("clear",))

    def expect(self, expression: bytes, timeout: float) -> None:
        self.calls.append(("expect", expression, timeout))


def test_blackout_transitions_preserve_output_received_while_attaching() -> None:
    console = RecordingConsole()

    RUNNER.enter_blackout(console)
    RUNNER.leave_blackout_and_verify_recovery(console)

    for index, call in enumerate(console.calls[:-1]):
        if call[0] == "attach":
            assert console.calls[index + 1][0] == "expect"
