import importlib.util
import sys
import types
import unittest
from pathlib import Path
from unittest import mock


RUNNER_PATH = Path(__file__).resolve().parents[2] / "board/run-atk-task2-fault.py"


def load_runner():
    spec = importlib.util.spec_from_file_location("run_atk_task2_fault", RUNNER_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    with mock.patch.dict(sys.modules, {"serial": types.ModuleType("serial")}):
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


class RunAtKTask2FaultTests(unittest.TestCase):
    def test_validator_accepts_remote_error_and_recovery(self) -> None:
        runner = load_runner()
        runner.validate_evidence(complete_log(), "invalid-parameter")

    def test_validator_rejects_missing_remote_error(self) -> None:
        runner = load_runner()
        with self.assertRaisesRegex(RuntimeError, "REMOTE_ERROR"):
            runner.validate_evidence(
                complete_log().replace(
                    b"STARRY_T2N1_REMOTE_ERROR code=InvalidParameter sequence=1\n",
                    b"",
                ),
                "invalid-parameter",
            )

    def test_validator_rejects_fatal_marker(self) -> None:
        runner = load_runner()
        with self.assertRaisesRegex(RuntimeError, "fatal marker"):
            runner.validate_evidence(
                complete_log() + b"\nESR_EL2=0x1", "invalid-parameter"
            )
