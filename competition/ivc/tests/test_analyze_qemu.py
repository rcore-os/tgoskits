from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


ANALYZER_PATH = Path(__file__).resolve().parents[1] / "analyze_qemu.py"
SPEC = importlib.util.spec_from_file_location("ivc_analyze_qemu", ANALYZER_PATH)
assert SPEC is not None and SPEC.loader is not None
analyzer = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = analyzer
SPEC.loader.exec_module(analyzer)


class AnalyzeQemuTests(unittest.TestCase):
    def test_accepts_complete_neural_run(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(valid_log(), encoding="utf-8")

            result = analyzer.analyze(log, 1800)

            self.assertEqual(result["controller"]["policy"], "neural")
            self.assertEqual(result["controller"]["full_loop_p99_us"], 4992)
            self.assertEqual(result["rtos"]["accepted"], 1800)
            self.assertEqual(len(result["source_log"]["sha256"]), 64)

    def test_rejects_application_errors(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(valid_log().replace("errors=0", "errors=1"), encoding="utf-8")

            with self.assertRaisesRegex(analyzer.AnalysisError, "errors or timeouts"):
                analyzer.analyze(log, 1800)

    def test_rejects_missing_linux_completion(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(valid_log().replace("IVC-LINUX-DONE exit=0\n", ""), encoding="utf-8")

            with self.assertRaisesRegex(analyzer.AnalysisError, "missing terminal marker"):
                analyzer.analyze(log, 1800)

    def test_accepts_exact_ack_loss_recovery_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(valid_ack_loss_log(), encoding="utf-8")

            result = analyzer.analyze(
                log,
                12,
                profile="ack-loss",
                drop_ack_every=5,
            )

            self.assertEqual(result["profile"], "ack-loss")
            self.assertEqual(result["controller"]["retransmissions"], 2)
            self.assertEqual(result["rtos"]["applied"], 12)
            self.assertEqual(result["rtos"]["duplicates"], 2)
            self.assertEqual(result["rtos"]["acks_dropped"], 2)
            self.assertEqual(result["rtos"]["injected_sequences"], [5, 10])
            self.assertEqual(len(result["source_log"]["sha256"]), 64)

    def test_accepts_ack_loss_when_final_command_requires_recovery(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(valid_terminal_ack_loss_log(), encoding="utf-8")

            result = analyzer.analyze(
                log,
                10,
                profile="ack-loss",
                drop_ack_every=5,
            )

            self.assertEqual(result["rtos"]["progress_duplicates"], 1)
            self.assertEqual(result["rtos"]["duplicates"], 2)
            self.assertEqual(result["rtos"]["acks_sent"], 10)

    def test_ack_loss_profile_rejects_missing_duplicate_recovery(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(
                valid_ack_loss_log().replace(
                    "IVC-RTOS-DUPLICATE seq=10 next_expected=11 duplicates=2\n",
                    "",
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(analyzer.AnalysisError, "duplicate sequence set"):
                analyzer.analyze(
                    log,
                    12,
                    profile="ack-loss",
                    drop_ack_every=5,
                )

    def test_ack_loss_profile_rejects_terminal_result_before_final_recovery(self) -> None:
        result_line = (
            "IVC-RTOS-RESULT profile=ack-loss accepted=10 applied=10 duplicates=2 "
            "acks_dropped=2 status_sent=12 acks_sent=10 errors_sent=0 "
            "protocol_errors=0\n"
        )
        duplicate_line = (
            "IVC-RTOS-DUPLICATE seq=10 next_expected=11 duplicates=2\n"
        )
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(
                valid_terminal_ack_loss_log().replace(
                    duplicate_line + result_line,
                    result_line + duplicate_line,
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(analyzer.AnalysisError, "evidence interval"):
                analyzer.analyze(
                    log,
                    10,
                    profile="ack-loss",
                    drop_ack_every=5,
                )

    def test_ack_loss_profile_rejects_terminal_result_before_final_fresh_command(self) -> None:
        progress_line = (
            "IVC-RTOS-PROGRESS accepted=12 seq=12 mode=Neural "
            "actuator_permille=417 measured_milli_c=49324 duplicates=2 "
            "protocol_errors=0\n"
        )
        result_line = (
            "IVC-RTOS-RESULT profile=ack-loss accepted=12 applied=12 duplicates=2 "
            "acks_dropped=2 status_sent=14 acks_sent=12 errors_sent=0 "
            "protocol_errors=0\n"
        )
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(
                valid_ack_loss_log().replace(
                    progress_line + result_line,
                    result_line + progress_line,
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(analyzer.AnalysisError, "not ordered"):
                analyzer.analyze(
                    log,
                    12,
                    profile="ack-loss",
                    drop_ack_every=5,
                )

    def test_normal_profile_rejects_ack_loss_run(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(valid_ack_loss_log(), encoding="utf-8")

            with self.assertRaises(analyzer.AnalysisError):
                analyzer.analyze(log, 12)

    def test_ack_loss_profile_rejects_counter_drift(self) -> None:
        mutations = {
            "ready interval": ("ack_loss_drop_every=5", "ack_loss_drop_every=4"),
            "ready count": ("expected_commands=12", "expected_commands=13"),
            "injected sequence": ("drop_ack_seq=10", "drop_ack_seq=11"),
            "retransmissions": ("retransmissions=2", "retransmissions=1"),
            "recoveries": ("recoveries=2", "recoveries=1"),
            "applied": ("accepted=12 applied=12", "accepted=12 applied=11"),
            "acks dropped": ("acks_dropped=2", "acks_dropped=1"),
            "status sent": ("status_sent=14", "status_sent=13"),
            "acks sent": ("acks_sent=12", "acks_sent=11"),
            "errors sent": ("errors_sent=0", "errors_sent=1"),
            "protocol errors": (
                "errors_sent=0 protocol_errors=0",
                "errors_sent=0 protocol_errors=1",
            ),
            "success": ("success_percent=100.000", "success_percent=99.000"),
        }
        for name, (before, after) in mutations.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary_dir:
                log = Path(temporary_dir) / "qemu.log"
                log.write_text(valid_ack_loss_log().replace(before, after), encoding="utf-8")

                with self.assertRaises(analyzer.AnalysisError):
                    analyzer.analyze(
                        log,
                        12,
                        profile="ack-loss",
                        drop_ack_every=5,
                    )

    def test_normal_profile_rejects_retransmission_metrics(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(
                valid_log().replace(
                    "retransmissions=0 recoveries=0",
                    "retransmissions=1 recoveries=1",
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(analyzer.AnalysisError, "retransmissions"):
                analyzer.analyze(log, 1800)

    def test_normal_profile_rejects_fault_evidence_markers(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            log = Path(temporary_dir) / "qemu.log"
            log.write_text(
                valid_log().replace(
                    "IVC-LINUX-DONE exit=0\n",
                    "IVC-RTOS-INJECT drop_ack_seq=5\nIVC-LINUX-DONE exit=0\n",
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(analyzer.AnalysisError, "ACK-loss evidence"):
                analyzer.analyze(log, 1800)


def valid_log() -> str:
    return """\
\x1b[mIVC-RTOS-PROGRESS accepted=1800 seq=1800 mode=Neural actuator_permille=417 measured_milli_c=49324 duplicates=0 protocol_errors=0
IVC-CONTROLLER-RESULT policy=neural sent=1800 acknowledged=1800 errors=0 timeouts=0 retransmissions=0 recoveries=0 success_percent=100.000 full_loop_p50_us=3767 full_loop_p95_us=4429 full_loop_p99_us=4992 full_loop_max_us=23154 pre_send_p50_us=10 pre_send_p95_us=12 pre_send_p99_us=23 pre_send_max_us=331 transport_p50_us=3756 transport_p95_us=4419 transport_p99_us=4981 transport_max_us=22823 throughput_msg_s=9.963 rmse_milli_c=5932.491 iae_milli_c_s=686993.400 max_overshoot_milli_c=13428
IVC-LINUX-DONE exit=0
"""


def valid_ack_loss_log() -> str:
    return """\
IVC-RTOS-READY bind=10.0.0.2:5500 mac=52:54:00:00:00:02 window_bits=64 ack_loss_drop_every=5 expected_commands=12
IVC-RTOS-INJECT drop_ack_seq=5
IVC-RTOS-DUPLICATE seq=5 next_expected=6 duplicates=1
IVC-RTOS-INJECT drop_ack_seq=10
IVC-RTOS-DUPLICATE seq=10 next_expected=11 duplicates=2
IVC-RTOS-PROGRESS accepted=12 seq=12 mode=Neural actuator_permille=417 measured_milli_c=49324 duplicates=2 protocol_errors=0
IVC-RTOS-RESULT profile=ack-loss accepted=12 applied=12 duplicates=2 acks_dropped=2 status_sent=14 acks_sent=12 errors_sent=0 protocol_errors=0
IVC-CONTROLLER-RESULT policy=neural sent=12 acknowledged=12 errors=0 timeouts=0 retransmissions=2 recoveries=2 success_percent=100.000 full_loop_p50_us=3767 full_loop_p95_us=104429 full_loop_p99_us=105992 full_loop_max_us=106154 pre_send_p50_us=10 pre_send_p95_us=12 pre_send_p99_us=23 pre_send_max_us=331 transport_p50_us=3756 transport_p95_us=104419 transport_p99_us=105981 transport_max_us=106823 throughput_msg_s=8.963 rmse_milli_c=5932.491 iae_milli_c_s=686993.400 max_overshoot_milli_c=13428
IVC-LINUX-DONE exit=0
"""


def valid_terminal_ack_loss_log() -> str:
    return """\
IVC-RTOS-READY bind=10.0.0.2:5500 mac=52:54:00:00:00:02 window_bits=64 ack_loss_drop_every=5 expected_commands=10
IVC-RTOS-INJECT drop_ack_seq=5
IVC-RTOS-DUPLICATE seq=5 next_expected=6 duplicates=1
IVC-RTOS-PROGRESS accepted=10 seq=10 mode=Neural actuator_permille=417 measured_milli_c=49324 duplicates=1 protocol_errors=0
IVC-RTOS-INJECT drop_ack_seq=10
IVC-RTOS-DUPLICATE seq=10 next_expected=11 duplicates=2
IVC-RTOS-RESULT profile=ack-loss accepted=10 applied=10 duplicates=2 acks_dropped=2 status_sent=12 acks_sent=10 errors_sent=0 protocol_errors=0
IVC-CONTROLLER-RESULT policy=neural sent=10 acknowledged=10 errors=0 timeouts=0 retransmissions=2 recoveries=2 success_percent=100.000 full_loop_p50_us=3767 full_loop_p95_us=104429 full_loop_p99_us=105992 full_loop_max_us=106154 pre_send_p50_us=10 pre_send_p95_us=12 pre_send_p99_us=23 pre_send_max_us=331 transport_p50_us=3756 transport_p95_us=104419 transport_p99_us=105981 transport_max_us=106823 throughput_msg_s=8.963 rmse_milli_c=5932.491 iae_milli_c_s=686993.400 max_overshoot_milli_c=13428
IVC-LINUX-DONE exit=0
"""


if __name__ == "__main__":
    unittest.main()
