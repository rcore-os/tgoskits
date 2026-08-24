#!/usr/bin/env python3
"""Deterministic tests for StarryOS Task-2/Task-3 evidence checks."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("verify_starry_task23.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("verify_starry_task23", MODULE_PATH)
assert SPEC and SPEC.loader
VERIFY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = VERIFY
SPEC.loader.exec_module(VERIFY)


def frame(
    *,
    src: str,
    dst: str,
    kind: int,
    sequence: int = 0,
    acknowledgement: int = 0,
    error_code: int = 0,
    body: bytes = b"",
) -> VERIFY.WireFrame:
    return VERIFY.WireFrame(
        src=src,
        dst=dst,
        kind=kind,
        sequence=sequence,
        acknowledgement=acknowledgement,
        error_code=error_code,
        body=body,
    )


class VerifyStarryTask23Tests(unittest.TestCase):
    def test_retry_exhaustion_requires_five_retries_and_no_ack(self) -> None:
        frames = [
            frame(
                src=VERIFY.STARRY_IP,
                dst=VERIFY.ZEPHYR_IP,
                kind=VERIFY.KIND_CONTROL,
                sequence=1,
            )
            for _ in range(6)
        ]
        log = "\n".join(
            (
                "TASK2_FAULT_MODE mode=drop-ack-always",
                "TASK2_FAULT_DROP_ACK_ALWAYS seq=1",
                "STARRY_T2N1_RETRANSMIT seq=1 attempt=5",
                "STARRY_T2N1_SAFE source=protocol reason=RetryExhausted",
                "STARRY_T2N1_RECOVERED state=Active",
            )
        )

        self.assertEqual(VERIFY.verify_retry_exhausted(frames, log), [])

    def test_out_of_order_injection_is_proven_by_wire_capture(self) -> None:
        frames = [
            frame(
                src=VERIFY.STARRY_IP,
                dst=VERIFY.ZEPHYR_IP,
                kind=VERIFY.KIND_CONTROL,
                sequence=2,
            ),
            frame(
                src=VERIFY.ZEPHYR_IP,
                dst=VERIFY.STARRY_IP,
                kind=VERIFY.KIND_ERROR,
                acknowledgement=2,
                error_code=2,
            ),
        ]
        log = "\n".join(
            (
                "TASK2_PROTOCOL_ERROR out_of_order=2 expected=1",
                "STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=out-of-order "
                "safe_observed=true recovered=true",
                "STARRY_T2N1_PASS",
            )
        )

        self.assertEqual(VERIFY.verify_out_of_order(frames, log), [])

    def test_invalid_parameter_injection_is_proven_by_wire_capture(self) -> None:
        payload = bytearray(12)
        payload[4:8] = (1001).to_bytes(4, "big", signed=True)
        frames = [
            frame(
                src=VERIFY.STARRY_IP,
                dst=VERIFY.ZEPHYR_IP,
                kind=VERIFY.KIND_CONTROL,
                sequence=1,
                body=bytes(payload),
            ),
            frame(
                src=VERIFY.ZEPHYR_IP,
                dst=VERIFY.STARRY_IP,
                kind=VERIFY.KIND_ERROR,
                acknowledgement=1,
                error_code=1,
            ),
        ]
        log = "\n".join(
            (
                "TASK2_PROTOCOL_ERROR invalid_parameter seq=1",
                "STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=invalid-parameter "
                "safe_observed=true recovered=true",
                "STARRY_T2N1_PASS",
            )
        )

        self.assertEqual(VERIFY.verify_invalid_parameter(frames, log), [])

    def test_blackout_requires_both_safe_states_and_completed_recovery_cycle(self) -> None:
        frames = [
            frame(
                src=VERIFY.STARRY_IP,
                dst=VERIFY.ZEPHYR_IP,
                kind=VERIFY.KIND_CONTROL,
                sequence=1,
            ),
            frame(
                src=VERIFY.STARRY_IP,
                dst=VERIFY.ZEPHYR_IP,
                kind=VERIFY.KIND_CONTROL,
                sequence=2,
            ),
        ]
        incomplete_log = "\n".join(
            (
                "virtnet: blackout ON",
                "STARRY_T2N1_SAFE source=protocol reason=RetryExhausted",
                "virtnet: blackout OFF",
                "STARRY_T2N1_RECOVERED state=Active",
                "STARRY_T2N1_STATUS_DELIVERED request=3",
            )
        )

        self.assertTrue(VERIFY.verify_blackout(frames, incomplete_log))

    def test_blackout_accepts_control_markers_before_and_after_recovery(self) -> None:
        frames = [
            frame(
                src=VERIFY.STARRY_IP,
                dst=VERIFY.ZEPHYR_IP,
                kind=VERIFY.KIND_CONTROL,
                sequence=1,
            ),
            frame(
                src=VERIFY.ZEPHYR_IP,
                dst=VERIFY.STARRY_IP,
                kind=VERIFY.KIND_STATUS,
                sequence=1,
            ),
            frame(
                src=VERIFY.STARRY_IP,
                dst=VERIFY.ZEPHYR_IP,
                kind=VERIFY.KIND_CONTROL,
                sequence=1,
            ),
            frame(
                src=VERIFY.ZEPHYR_IP,
                dst=VERIFY.STARRY_IP,
                kind=VERIFY.KIND_STATUS,
                sequence=1,
            ),
        ]
        complete_log = "\n".join(
            (
                "TASK2_CONTROL_RECEIVED seq=1 request=1",
                "virtnet: blackout ON",
                "STARRY_T2N1_SAFE source=protocol reason=RetryExhausted",
                "TASK2_SAFE state=Safe event=HeartbeatTimeout",
                "virtnet: blackout OFF",
                "STARRY_T2N1_RECOVERED state=Active",
                "TASK3_INFER model=yolo11n.ncnn infer_us=13000000 request=3",
                "STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=normal "
                "safe_observed=true recovered=true",
                "TASK2_CONTROL_RECEIVED seq=1 request=3",
            )
        )

        self.assertEqual(VERIFY.verify_blackout(frames, complete_log), [])

    def test_yolo_model_rejection_keeps_heartbeat_but_emits_no_control(self) -> None:
        frames = [
            frame(
                src=VERIFY.STARRY_IP,
                dst=VERIFY.ZEPHYR_IP,
                kind=VERIFY.KIND_HEARTBEAT,
            )
        ]
        log = "\n".join(
            (
                "TASK3_MODEL_READY model=yolo11n.ncnn runtime=ncnn "
                "mode=in-guest run_mode=model-rejected",
                "TASK3_MODEL_REJECTED model=yolo11n.ncnn "
                "reason=InjectedInvalidOutput action=safe",
                "STARRY_T2N1_SAFE source=model reason=InjectedInvalidOutput",
            )
        )

        self.assertEqual(VERIFY.verify_model_rejected(frames, log), [])


if __name__ == "__main__":
    unittest.main()
