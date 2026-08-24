import importlib.util
from pathlib import Path

import pytest


RUNNER_PATH = Path(__file__).resolve().parents[2] / "board/run-atk-task2-fault.py"


def load_runner():
    spec = importlib.util.spec_from_file_location("run_atk_task2_fault", RUNNER_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def complete_log() -> bytes:
    return b"\n".join(
        (
            b"STARRY_T2N1_FAULT_SENT mode=invalid-parameter seq=1 request=1",
            b"STARRY_T2N1_REMOTE_ERROR code=InvalidParameter sequence=1",
            b"STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=invalid-parameter request=2 "
            b"safe_observed=true recovered=true",
            b"TASK2_PROTOCOL_ERROR invalid_parameter seq=1",
            b"atk-task123-starry running",
            b"atk-task123-zephyr running",
        )
    )


def test_validator_accepts_remote_error_and_recovery() -> None:
    runner = load_runner()
    runner.validate_evidence(complete_log(), "invalid-parameter")


def test_validator_rejects_missing_remote_error() -> None:
    runner = load_runner()
    with pytest.raises(RuntimeError, match="REMOTE_ERROR"):
        runner.validate_evidence(
            complete_log().replace(
                b"STARRY_T2N1_REMOTE_ERROR code=InvalidParameter sequence=1\n", b""
            ),
            "invalid-parameter",
        )


def test_validator_rejects_fatal_marker() -> None:
    runner = load_runner()
    with pytest.raises(RuntimeError, match="fatal marker"):
        runner.validate_evidence(complete_log() + b"\nESR_EL2=0x1", "invalid-parameter")
