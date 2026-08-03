from __future__ import annotations

import importlib.util
import hashlib
import gzip
import sys
import tempfile
import unittest
from pathlib import Path


ANALYZER_PATH = Path(__file__).resolve().parents[1] / "analyze_board.py"
SPEC = importlib.util.spec_from_file_location("ivc_analyze_board", ANALYZER_PATH)
assert SPEC is not None and SPEC.loader is not None
analyzer = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = analyzer
SPEC.loader.exec_module(analyzer)


VALID_LOG = """\
[guest-console:pl011-starry] IVC-STARRY-BOOT mode=neural count=1800 period_ms=100 vcpus=2
[guest-console:pl011-starry] IVC-STARRY-NET iface=eth0 mac=02:00:00:00:00:01 ip=10.0.0.1/24 peer=10.0.0.2 udp_port=5500 segment=1
[guest-console:pl011-starry] IVC-CONTROLLER-OUTCOME policy=neural sent=1800 acknowledged=1800 errors=0 timeouts=0
[guest-console:pl011-starry] IVC-CONTROLLER-RELIABILITY retransmissions=0 recoveries=0 success_percent=100.000
[guest-console:pl011-starry] IVC-CONTROLLER-FULL-LOOP p50_us=6644 p95_us=11282 p99_us=11719 max_us=20115
[guest-console:pl011-starry] IVC-CONTROLLER-PRE-SEND p50_us=17 p95_us=17 p99_us=17 max_us=365
[guest-console:pl011-starry] IVC-CONTROLLER-TRANSPORT p50_us=6628 p95_us=11266 p99_us=11702 max_us=20098 throughput_msg_s=9.995
[guest-console:pl011-starry] IVC-CONTROLLER-CONTROL rmse_milli_c=5932.491 iae_milli_c_s=686993.400 max_overshoot_milli_c=13428
[guest-console:pl011-starry] IVC-CONTROLLER-RESULT policy=neural sent=1800 acknowledged=1800 trasg_s=9.995
[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME profile=normal accepted=1800 applied=1800 duplicates=0 acks_dropped=0
[guest-console:pl011-zephyr] IVC-RTOS-MESSAGES status_sent=1800 acks_sent=1800 errors_sent=0 protocol_errors=0
[guest-console:pl011-zephyr] IVC-RTOS-RESULT profile=normal accepted=1800 applied=1800 duplicates=0 acks_dropped=0 status_sent=1800 acks_sent=1800 errors_sent=0 protocol_errors=0
[guest-console:pl011-zephyr] IVC-RTOS-POWEROFF accepted=1800
[guest-console:pl011-starry] IVC-STARRY-DONE exit=0
AXVISOR_SNAPSHOT_SYNC_OK
BOARD_LINUX_RESTORED
BOARD_RESULT_IMAGE_VALIDATED vm=1 index=0 path=/home/orangepi/axvisor-guest/starry-ivc-rootfs.result.img bytes=67108864 sha256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef fsck=clean
"""

RAW_CSV = """\
sequence,cycle_started_us,command_sent_us,response_completed_us,full_loop_us,pre_send_us,transport_us,setpoint_milli_c,observed_milli_c,measured_milli_c,command_actuator_permille,status_actuator_permille,error_milli_c
1,0,10,110,110,10,100,45000,44000,44000,500,500,1000
2,200,220,340,140,20,120,45000,44000,46000,500,500,-1000
3,400,430,580,180,30,150,45000,46000,45000,500,500,0
4,600,640,840,240,40,200,45000,45000,43000,500,500,2000
"""

RAW_LOG_TEMPLATE = """\
[guest-console:pl011-starry] IVC-STARRY-BOOT mode=neural backend=native count=4 period_ms=100 vcpus=2
[guest-console:pl011-starry] IVC-STARRY-NET iface=eth0 mac=02:00:00:00:00:01 ip=10.0.0.1/24 peer=10.0.0.2 udp_port=5500 segment=1
[guest-console:pl011-starry] IVC-CONTROLLER-OUTCOME policy=neural sent=4 acknowledged=4 errors=0 timeouts=0
[guest-console:pl011-starry] IVC-CONTROLLER-RELIABILITY retransmissions=0 recoveries=0 success_percent=100.000
[guest-console:pl011-starry] IVC-CONTROLLER-FULL-LOOP p50_us=140 p95_us=180 p99_us=180 max_us=240
[guest-console:pl011-starry] IVC-CONTROLLER-PRE-SEND p50_us=20 p95_us=30 p99_us=30 max_us=40
[guest-console:pl011-starry] IVC-CONTROLLER-TRANSPORT p50_us=120 p95_us=150 p99_us=150 max_us=200 throughput_msg_s=9.000
[guest-console:pl011-starry] IVC-CONTROLLER-CONTROL rmse_milli_c=1224.745 iae_milli_c_s=400.000 max_overshoot_milli_c=1000
[guest-console:pl011-starry] IVC-STARRY-RAW path=/var/lib/ivc/raw.csv samples=4 sha256={raw_sha256}
[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME profile=normal accepted=4 applied=4 duplicates=0 acks_dropped=0
[guest-console:pl011-zephyr] IVC-RTOS-MESSAGES status_sent=4 acks_sent=4 errors_sent=0 protocol_errors=0
[guest-console:pl011-zephyr] IVC-RTOS-POWEROFF accepted=4
[guest-console:pl011-starry] IVC-STARRY-DONE exit=0
AXVISOR_SNAPSHOT_SYNC_OK
BOARD_LINUX_RESTORED
BOARD_RESULT_IMAGE_VALIDATED vm=1 index=0 path=/home/orangepi/axvisor-guest/starry-ivc-rootfs.result.img bytes=67108864 sha256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef fsck=clean
BOARD_GUEST_RAW_MANIFEST path=/var/lib/ivc/raw.csv samples=4 sha256={raw_sha256}
BOARD_RAW_RESULT_HARVESTED path=/tmp/results/raw.csv samples=4 sha256={raw_sha256}
BOARD_IDENTITY board_id=test-rk3588 hostname=orangepi5plus cpu_temp_milli_c=42500
"""


class BoardAnalysisTests(unittest.TestCase):
    def write_log(self, contents: str) -> Path:
        temporary = tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", delete=False)
        self.addCleanup(Path(temporary.name).unlink, missing_ok=True)
        with temporary:
            temporary.write(contents)
        return Path(temporary.name)

    def write_raw_csv(self, contents: str = RAW_CSV) -> Path:
        temporary = tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", delete=False)
        self.addCleanup(Path(temporary.name).unlink, missing_ok=True)
        with temporary:
            temporary.write(contents)
        return Path(temporary.name)

    def write_gzip(self, contents: str) -> Path:
        temporary = tempfile.NamedTemporaryFile(suffix=".gz", delete=False)
        temporary.close()
        path = Path(temporary.name)
        self.addCleanup(path.unlink, missing_ok=True)
        with gzip.open(path, "wt", encoding="utf-8") as output:
            output.write(contents)
        return path

    def raw_log(self, raw_csv: str = RAW_CSV) -> str:
        digest = hashlib.sha256(raw_csv.encode()).hexdigest()
        return RAW_LOG_TEMPLATE.format(raw_sha256=digest)

    def ack_loss_log(self, raw_csv: str = RAW_CSV) -> str:
        normal_outcome = (
            "[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME profile=normal "
            "accepted=4 applied=4 duplicates=0 acks_dropped=0\n"
        )
        fault_outcome = (
            "[guest-console:pl011-zephyr] IVC-RTOS-READY "
            "bind=10.0.0.2:5500 mac=52:54:00:00:00:02 window_bits=64 "
            "ack_loss_drop_every=2 expected_commands=4 exit_after_expected=1\n"
            "[guest-console:pl011-zephyr] IVC-RTOS-INJECT drop_ack_seq=2\n"
            "[guest-console:pl011-zephyr] IVC-RTOS-DUPLICATE "
            "seq=2 next_expected=3 duplicates=1\n"
            "[guest-console:pl011-zephyr] IVC-RTOS-INJECT drop_ack_seq=4\n"
            "[guest-console:pl011-zephyr] IVC-RTOS-DUPLICATE "
            "seq=4 next_expected=5 duplicates=2\n"
            "[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME profile=ack-loss "
            "accepted=4 applied=4 duplicates=2 acks_dropped=2\n"
        )
        return (
            self.raw_log(raw_csv)
            .replace(
                "IVC-CONTROLLER-RELIABILITY retransmissions=0 recoveries=0",
                "IVC-CONTROLLER-RELIABILITY retransmissions=2 recoveries=2",
            )
            .replace(normal_outcome, fault_outcome)
            .replace(
                "IVC-RTOS-MESSAGES status_sent=4 acks_sent=4",
                "IVC-RTOS-MESSAGES status_sent=6 acks_sent=4",
            )
            .replace(
                "/home/orangepi/axvisor-guest/starry-ivc-rootfs.result.img",
                "/home/orangepi/ivc-a",
            )
        )

    def error_profile_log(self, raw_csv: str = RAW_CSV) -> str:
        normal_outcome = (
            "[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME profile=normal "
            "accepted=4 applied=4 duplicates=0 acks_dropped=0\n"
        )
        fault_records = """\
[guest-console:pl011-zephyr] IVC-RTOS-READY bind=10.0.0.2:5500 mac=52:54:00:00:00:02 window_bits=64 ack_loss_drop_every=0 expected_commands=4 expected_protocol_errors=5 exit_after_expected=1
[guest-console:pl011-starry] IVC-CONTROLLER-FAULT kind=unsupported-version seq=1001 expected_code=2 observed_code=2
[guest-console:pl011-zephyr] IVC-RTOS-ERROR seq=1001 code=2 reason=unsupported-version
[guest-console:pl011-starry] IVC-CONTROLLER-FAULT kind=length-mismatch seq=1002 expected_code=1 observed_code=1
[guest-console:pl011-zephyr] IVC-RTOS-ERROR seq=1002 code=1 reason=length-mismatch
[guest-console:pl011-starry] IVC-CONTROLLER-FAULT kind=checksum-mismatch seq=1003 expected_code=3 observed_code=3
[guest-console:pl011-zephyr] IVC-RTOS-ERROR seq=1003 code=3 reason=checksum-mismatch
[guest-console:pl011-starry] IVC-CONTROLLER-FAULT kind=unexpected-message-type seq=1004 expected_code=5 observed_code=5
[guest-console:pl011-zephyr] IVC-RTOS-ERROR seq=1004 code=5 reason=unexpected-message-type
[guest-console:pl011-starry] IVC-CONTROLLER-FAULT kind=invalid-session-transition seq=1005 expected_code=4 observed_code=4
[guest-console:pl011-zephyr] IVC-RTOS-ERROR seq=1005 code=4 reason=zero-session-or-sequence
[guest-console:pl011-starry] IVC-CONTROLLER-FAULT-RESULT profile=error injected=5 errors_received=5 normal_acknowledged=4 continued=1
[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME profile=error accepted=4 applied=4 duplicates=0 acks_dropped=0
"""
        return (
            self.raw_log(raw_csv)
            .replace(
                "backend=native count=4",
                "backend=native fault_profile=error count=4",
            )
            .replace(normal_outcome, fault_records)
            .replace(
                "IVC-RTOS-MESSAGES status_sent=4 acks_sent=4 errors_sent=0 "
                "protocol_errors=0",
                "IVC-RTOS-MESSAGES status_sent=4 acks_sent=4 errors_sent=5 "
                "protocol_errors=5",
            )
            .replace(
                "/home/orangepi/axvisor-guest/starry-ivc-rootfs.result.img",
                "/home/orangepi/ivc-e",
            )
        )

    def test_compact_records_survive_a_corrupted_legacy_result(self) -> None:
        result = analyzer.analyze(self.write_log(VALID_LOG), 1_800)

        self.assertEqual(result["platform"], "orangepi-5-plus")
        self.assertEqual(result["guest"], "starryos")
        self.assertEqual(result["controller"]["acknowledged"], 1_800)
        self.assertEqual(result["controller"]["transport_p99_us"], 11_702)
        self.assertEqual(result["rtos"]["applied"], 1_800)
        self.assertTrue(result["lifecycle"]["host_filesystem_synced"])
        self.assertTrue(result["lifecycle"]["board_linux_restored"])

    def test_complete_legacy_result_recovers_dropped_compact_outcome(self) -> None:
        log = VALID_LOG.replace(
            "[guest-console:pl011-starry] IVC-CONTROLLER-OUTCOME "
            "policy=neural sent=1800 acknowledged=1800 errors=0 timeouts=0\n",
            "",
        ).replace(
            "[guest-console:pl011-starry] IVC-CONTROLLER-RELIABILITY "
            "retransmissions=0 recoveries=0 success_percent=100.000\n",
            "",
        ).replace(
            "[guest-console:pl011-starry] IVC-CONTROLLER-RESULT "
            "policy=neural sent=1800 acknowledged=1800 trasg_s=9.995\n",
            "[guest-console:pl011-starry] IVC-CONTROLLER-RESULT "
            "policy=neural sent=1800 acknowledged=1800 errors=0 timeouts=0 "
            "retransmissions=0 recoveries=0 success_percent=100.000\n",
        )

        result = analyzer.analyze(self.write_log(log), 1_800)

        self.assertEqual(result["controller"]["acknowledged"], 1_800)
        self.assertEqual(result["controller"]["recoveries"], 0)

    def test_conflicting_complete_legacy_outcome_is_rejected(self) -> None:
        log = VALID_LOG.replace(
            "[guest-console:pl011-starry] IVC-CONTROLLER-RESULT "
            "policy=neural sent=1800 acknowledged=1800 trasg_s=9.995\n",
            "[guest-console:pl011-starry] IVC-CONTROLLER-RESULT "
            "policy=neural sent=1799 acknowledged=1799 errors=0 timeouts=0 "
            "retransmissions=0 recoveries=0 success_percent=100.000\n",
        )

        with self.assertRaisesRegex(analyzer.AnalysisError, "conflicting complete"):
            analyzer.analyze(self.write_log(log), 1_800)

    def test_redundant_compact_record_survives_one_uart_damaged_copy(self) -> None:
        complete = (
            "[guest-console:pl011-starry] IVC-CONTROLLER-PRE-SEND "
            "p50_us=17 p95_us=17 p99_us=17 max_us=365\n"
        )
        damaged = (
            "[guest-console:pl011-starry] IVC-CONTROLLER-PRE-SEND "
            "p50_us=17 p95_us=17 p99_us=17 max_"
            "[guest-console:pl011-starry] IVC-CONTROLLER-TRANSPORT\n"
        )
        log = VALID_LOG.replace(complete, damaged + complete)

        result = analyzer.analyze(self.write_log(log), 1_800)

        self.assertEqual(result["controller"]["pre_send_max_us"], 365)

    def test_concatenated_guest_records_are_split_at_uart_prefix(self) -> None:
        log = VALID_LOG.replace(
            "acks_dropped=0\n"
            "[guest-console:pl011-zephyr] IVC-RTOS-MESSAGES",
            "acks_dropped=0"
            "[guest-console:pl011-zephyr] IVC-RTOS-MESSAGES",
            1,
        )

        result = analyzer.analyze(self.write_log(log), 1_800)

        self.assertEqual(result["rtos"]["acks_sent"], 1_800)

    def test_runner_sync_confirmation_survives_a_damaged_raw_marker(self) -> None:
        log = VALID_LOG.replace(
            "AXVISOR_SNAPSHOT_SYNC_OK\n",
            "AXVISOR_SNAPSHOT_SYNC_OK\n"
            "[HOST_FILESYSTEM_SYNCED\n",
        )

        result = analyzer.analyze(self.write_log(log), 1_800)

        self.assertTrue(result["lifecycle"]["host_filesystem_synced"])

    def test_missing_linux_restore_is_rejected(self) -> None:
        log = VALID_LOG.replace("BOARD_LINUX_RESTORED\n", "")

        with self.assertRaisesRegex(analyzer.AnalysisError, "BOARD_LINUX_RESTORED"):
            analyzer.analyze(self.write_log(log), 1_800)

    def test_missing_volatile_block_snapshot_is_rejected(self) -> None:
        log = VALID_LOG.replace(
            "BOARD_RESULT_IMAGE_VALIDATED vm=1 index=0 "
            "path=/home/orangepi/axvisor-guest/starry-ivc-rootfs.result.img "
            "bytes=67108864 "
            "sha256=0123456789abcdef0123456789abcdef"
            "0123456789abcdef0123456789abcdef fsck=clean\n",
            "",
        )

        with self.assertRaisesRegex(
            analyzer.AnalysisError, "BOARD_RESULT_IMAGE_VALIDATED"
        ):
            analyzer.analyze(self.write_log(log), 1_800)

    def test_compact_uart_safe_snapshot_path_is_accepted(self) -> None:
        compact_log = VALID_LOG.replace(
            "/home/orangepi/axvisor-guest/starry-ivc-rootfs.result.img",
            "/home/orangepi/ivc-n",
        )

        result = analyzer.analyze(self.write_log(compact_log), 1_800)

        self.assertEqual(
            result["lifecycle"]["block_snapshot"]["image_path"],
            "/home/orangepi/ivc-n",
        )

    def test_rtos_duplicates_are_rejected(self) -> None:
        log = VALID_LOG.replace("duplicates=0", "duplicates=1")

        with self.assertRaisesRegex(analyzer.AnalysisError, "duplicates"):
            analyzer.analyze(self.write_log(log), 1_800)

    def test_compact_rtos_records_do_not_depend_on_the_legacy_long_line(self) -> None:
        legacy = (
            "[guest-console:pl011-zephyr] IVC-RTOS-RESULT profile=normal "
            "accepted=1800 applied=1800 duplicates=0 acks_dropped=0 "
            "status_sent=1800 acks_sent=1800 errors_sent=0 protocol_errors=0\n"
        )

        result = analyzer.analyze(self.write_log(VALID_LOG.replace(legacy, "")), 1_800)

        self.assertEqual(result["rtos"]["accepted"], 1_800)

    def test_raw_samples_are_recomputed_and_cross_checked_with_console(self) -> None:
        raw_path = self.write_raw_csv()

        result = analyzer.analyze(self.write_log(self.raw_log()), 4, raw_path)

        self.assertEqual(result["raw_samples"]["sample_count"], 4)
        self.assertEqual(result["raw_samples"]["deadline_misses"], 0)
        self.assertEqual(result["board"]["board_id"], "test-rk3588")
        self.assertEqual(result["board"]["cpu_temp_milli_c"], 42_500)
        self.assertEqual(result["controller"]["full_loop_p99_us"], 180)
        self.assertAlmostEqual(
            result["controller"]["rmse_milli_c"], 1224.744871, places=6
        )

    def test_snapshot_guest_manifest_recovers_a_truncated_uart_hash(self) -> None:
        raw_path = self.write_raw_csv()
        digest = hashlib.sha256(RAW_CSV.encode()).hexdigest()
        uart_record = (
            "IVC-STARRY-RAW path=/var/lib/ivc/raw.csv samples=4 "
            f"sha256={digest}"
        )
        damaged_uart_record = uart_record.replace(digest, digest[:48])
        log = self.raw_log().replace(uart_record, damaged_uart_record)

        result = analyzer.analyze(self.write_log(log), 4, raw_path)

        self.assertEqual(result["raw_samples"]["guest_manifest_sha256"], digest)
        self.assertFalse(result["raw_samples"]["uart_sha256_complete"])

    def test_snapshot_guest_manifest_mismatch_is_rejected(self) -> None:
        digest = hashlib.sha256(RAW_CSV.encode()).hexdigest()
        log = self.raw_log().replace(
            "BOARD_GUEST_RAW_MANIFEST path=/var/lib/ivc/raw.csv samples=4 "
            f"sha256={digest}",
            "BOARD_GUEST_RAW_MANIFEST path=/var/lib/ivc/raw.csv samples=4 "
            f"sha256={'0' * 64}",
        )

        with self.assertRaisesRegex(analyzer.AnalysisError, "snapshot guest manifest"):
            analyzer.analyze(self.write_log(log), 4, self.write_raw_csv())

    def test_complete_conflicting_uart_hash_is_rejected(self) -> None:
        digest = hashlib.sha256(RAW_CSV.encode()).hexdigest()
        uart_record = (
            "IVC-STARRY-RAW path=/var/lib/ivc/raw.csv samples=4 "
            f"sha256={digest}"
        )
        log = self.raw_log().replace(
            uart_record,
            uart_record.replace(digest, "0" * 64),
        )

        with self.assertRaisesRegex(analyzer.AnalysisError, "UART SHA-256 conflicts"):
            analyzer.analyze(self.write_log(log), 4, self.write_raw_csv())

    def test_raw_samples_replace_missing_uart_metric_summaries(self) -> None:
        raw_path = self.write_raw_csv()
        metric_prefixes = (
            "IVC-CONTROLLER-FULL-LOOP ",
            "IVC-CONTROLLER-PRE-SEND ",
            "IVC-CONTROLLER-TRANSPORT ",
            "IVC-CONTROLLER-CONTROL ",
        )
        log = "\n".join(
            line
            for line in self.raw_log().splitlines()
            if not any(prefix in line for prefix in metric_prefixes)
        )

        result = analyzer.analyze(self.write_log(log + "\n"), 4, raw_path)

        self.assertEqual(result["controller"]["full_loop_p99_us"], 180)
        self.assertAlmostEqual(
            result["controller"]["rmse_milli_c"], 1224.744871, places=6
        )

    def test_raw_samples_replace_conflicting_uart_metric_summaries(self) -> None:
        raw_path = self.write_raw_csv()
        transport = (
            "[guest-console:pl011-starry] IVC-CONTROLLER-TRANSPORT "
            "p50_us=120 p95_us=150 p99_us=150 max_us=200 "
            "throughput_msg_s=9.000\n"
        )
        damaged_copy = transport.replace(
            "throughput_msg_s=9.000", "throughput_msg_s=9.0"
        )
        log = self.raw_log().replace(transport, damaged_copy + transport)

        result = analyzer.analyze(self.write_log(log), 4, raw_path)

        self.assertEqual(result["controller"]["transport_p50_us"], 120)
        self.assertEqual(result["controller"]["transport_max_us"], 200)

    def test_conflicting_uart_metric_summaries_without_raw_are_rejected(self) -> None:
        full_loop = (
            "[guest-console:pl011-starry] IVC-CONTROLLER-FULL-LOOP "
            "p50_us=6644 p95_us=11282 p99_us=11719 max_us=20115\n"
        )
        conflicting = full_loop.replace("p50_us=6644", "p50_us=6643")
        log = VALID_LOG.replace(full_loop, conflicting + full_loop)

        with self.assertRaisesRegex(analyzer.AnalysisError, "conflicting complete"):
            analyzer.analyze(self.write_log(log), 1_800)

    def test_tampered_raw_samples_are_rejected_by_guest_hash(self) -> None:
        raw_path = self.write_raw_csv(RAW_CSV.replace(",43000,500,500,2000", ",43001,500,500,1999"))

        with self.assertRaisesRegex(analyzer.AnalysisError, "SHA-256"):
            analyzer.analyze(self.write_log(self.raw_log()), 4, raw_path)

    def test_compressed_console_and_raw_artifacts_remain_analyzable(self) -> None:
        result = analyzer.analyze(
            self.write_gzip(self.raw_log()),
            4,
            self.write_gzip(RAW_CSV),
        )

        self.assertEqual(result["raw_samples"]["sample_count"], 4)
        self.assertEqual(
            result["raw_samples"]["sha256"],
            hashlib.sha256(RAW_CSV.encode()).hexdigest(),
        )

    def test_ack_loss_capture_cross_checks_every_injected_recovery(self) -> None:
        result = analyzer.analyze(
            self.write_log(self.ack_loss_log()),
            4,
            self.write_raw_csv(),
            profile="ack-loss",
            drop_ack_every=2,
        )

        self.assertEqual(result["profile"], "ack-loss")
        self.assertEqual(result["controller"]["retransmissions"], 2)
        self.assertEqual(result["controller"]["recoveries"], 2)
        self.assertEqual(result["rtos"]["injected_sequences"], [2, 4])
        self.assertEqual(result["rtos"]["duplicate_sequences"], [2, 4])
        self.assertEqual(result["rtos"]["status_sent"], 6)
        self.assertEqual(
            result["lifecycle"]["block_snapshot"]["image_path"],
            "/home/orangepi/ivc-a",
        )

    def test_ack_loss_capture_rejects_a_missing_injection_marker(self) -> None:
        log = self.ack_loss_log().replace(
            "[guest-console:pl011-zephyr] IVC-RTOS-INJECT drop_ack_seq=4\n",
            "",
        )

        with self.assertRaisesRegex(
            analyzer.AnalysisError, "injected ACK-loss sequence set"
        ):
            analyzer.analyze(
                self.write_log(log),
                4,
                self.write_raw_csv(),
                profile="ack-loss",
                drop_ack_every=2,
            )

    def test_error_profile_cross_checks_every_error_and_normal_continuation(self) -> None:
        result = analyzer.analyze(
            self.write_log(self.error_profile_log()),
            4,
            self.write_raw_csv(),
            profile="error",
        )

        self.assertEqual(result["profile"], "error")
        self.assertEqual(result["rtos"]["errors_sent"], 5)
        self.assertEqual(result["rtos"]["protocol_errors"], 5)
        self.assertEqual(
            [fault["kind"] for fault in result["error_evidence"]],
            [
                "unsupported-version",
                "length-mismatch",
                "checksum-mismatch",
                "unexpected-message-type",
                "invalid-session-transition",
            ],
        )
        self.assertTrue(result["error_recovery"]["continued"])
        self.assertEqual(result["error_recovery"]["normal_acknowledged"], 4)

    def test_error_profile_rejects_a_missing_rtos_error_marker(self) -> None:
        log = self.error_profile_log().replace(
            "[guest-console:pl011-zephyr] IVC-RTOS-ERROR seq=1003 code=3 "
            "reason=checksum-mismatch\n",
            "",
        )

        with self.assertRaisesRegex(analyzer.AnalysisError, "error evidence"):
            analyzer.analyze(
                self.write_log(log),
                4,
                self.write_raw_csv(),
                profile="error",
            )

    def test_error_profile_rejects_a_non_error_starry_boot_profile(self) -> None:
        log = self.error_profile_log().replace(
            "fault_profile=error", "fault_profile=none"
        )

        with self.assertRaisesRegex(analyzer.AnalysisError, "fault profile"):
            analyzer.analyze(
                self.write_log(log),
                4,
                self.write_raw_csv(),
                profile="error",
            )

    def test_normal_capture_rejects_ack_loss_markers(self) -> None:
        log = self.raw_log().replace(
            "[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME",
            "[guest-console:pl011-zephyr] IVC-RTOS-INJECT drop_ack_seq=2\n"
            "[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME",
        )

        with self.assertRaisesRegex(analyzer.AnalysisError, "ACK-loss evidence"):
            analyzer.analyze(self.write_log(log), 4, self.write_raw_csv())


if __name__ == "__main__":
    unittest.main()
