from __future__ import annotations

import gzip
import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


IVC_DIR = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "ivc_aggregate_board_campaign",
    IVC_DIR / "aggregate_board_campaign.py",
)
assert SPEC is not None and SPEC.loader is not None
aggregate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = aggregate
SPEC.loader.exec_module(aggregate)


SOURCE_COMMIT = "a" * 40
ROOTFS_SHA256 = "b" * 64
BOARD_ID = "0123456789abcdef"
RUN_ORDER = ["run-001", "run-002", "run-003"]
INJECTED_SEQUENCES = list(range(5, 101, 5))
ERROR_FAULTS = [
    ("unsupported-version", 1001, 2, "unsupported-version"),
    ("length-mismatch", 1002, 1, "length-mismatch"),
    ("checksum-mismatch", 1003, 3, "checksum-mismatch"),
    ("unexpected-message-type", 1004, 5, "unexpected-message-type"),
    ("invalid-session-transition", 1005, 4, "zero-session-or-sequence"),
]
RAW_HEADER = (
    "sequence,cycle_started_us,command_sent_us,response_completed_us,"
    "full_loop_us,pre_send_us,transport_us,setpoint_milli_c,"
    "observed_milli_c,measured_milli_c,command_actuator_permille,"
    "status_actuator_permille,error_milli_c\n"
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def write_json(path: Path, value: object) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def raw_bytes(count: int = 100) -> bytes:
    rows = [RAW_HEADER]
    for sequence in range(1, count + 1):
        cycle = sequence * 100_000
        rows.append(
            f"{sequence},{cycle},{cycle + 10},{cycle + 1010},1010,10,1000,"
            "45000,20000,20000,900,900,25000\n"
        )
    return "".join(rows).encode()


def write_run(run_dir: Path, run_number: int) -> None:
    run_dir.mkdir(parents=True)
    raw = raw_bytes()
    console = f"formal ACK-loss run {run_number}\n".encode()
    raw_gzip = gzip.compress(raw, mtime=0)
    console_gzip = gzip.compress(console, mtime=0)
    (run_dir / "raw.csv").write_bytes(raw)
    (run_dir / "raw.csv.gz").write_bytes(raw_gzip)
    (run_dir / "console.log").write_bytes(console)
    (run_dir / "console.log.gz").write_bytes(console_gzip)

    raw_sha256 = sha256_bytes(raw)
    summary = {
        "schema_version": 2,
        "platform": "orangepi-5-plus",
        "guest": "starryos",
        "profile": "ack-loss",
        "board": {
            "board_id": BOARD_ID,
            "hostname": "orangepi5plus",
            "cpu_temp_milli_c": 42_000 + run_number,
        },
        "controller": {
            "policy": "neural",
            "sent": 100,
            "acknowledged": 100,
            "errors": 0,
            "timeouts": 0,
            "retransmissions": 20,
            "recoveries": 20,
            "deadline_misses": 20,
            "success_percent": 100.0,
            "full_loop_p50_us": 100 * run_number,
            "full_loop_p95_us": 200 * run_number,
            "full_loop_p99_us": 300 * run_number,
            "full_loop_max_us": 400 * run_number,
            "pre_send_p50_us": 10 * run_number,
            "pre_send_p95_us": 20 * run_number,
            "pre_send_p99_us": 30 * run_number,
            "pre_send_max_us": 40 * run_number,
            "transport_p50_us": 90 * run_number,
            "transport_p95_us": 180 * run_number,
            "transport_p99_us": 270 * run_number,
            "transport_max_us": 360 * run_number,
            "throughput_msg_s": 10.0 - run_number / 10,
            "rmse_milli_c": 23_000.0 + run_number,
            "iae_milli_c_s": 218_000.0 + run_number,
            "max_overshoot_milli_c": 0,
        },
        "rtos": {
            "profile": "ack-loss",
            "drop_ack_every": 5,
            "expected_recoveries": 20,
            "accepted": 100,
            "applied": 100,
            "duplicates": 20,
            "acks_dropped": 20,
            "status_sent": 120,
            "acks_sent": 100,
            "errors_sent": 0,
            "protocol_errors": 0,
            "injected_sequences": INJECTED_SEQUENCES,
            "duplicate_sequences": INJECTED_SEQUENCES,
        },
        "starry": {
            "mode": "neural",
            "count": 100,
            "period_ms": 100,
            "vcpus": 2,
        },
        "lifecycle": {
            "starry_done": True,
            "rtos_powered_off": True,
            "host_filesystem_synced": True,
            "volatile_block_snapshotted": True,
            "board_linux_restored": True,
            "block_snapshot": {"filesystem_check": "clean"},
        },
        "raw_samples": {
            "path": str(run_dir / "raw.csv.gz"),
            "sha256": raw_sha256,
            "artifact_sha256": sha256_bytes(raw_gzip),
            "guest_manifest_sha256": raw_sha256,
            "sample_count": 100,
            "dropped_samples": 0,
            "deadline_misses": 20,
        },
        "source_log": {
            "path": str(run_dir / "console.log.gz"),
            "sha256": sha256_bytes(console_gzip),
            "content_sha256": sha256_bytes(console),
        },
    }
    write_json(run_dir / "summary.json", summary)

    metadata = {
        "schema_version": 1,
        "source": {
            "branch": "experiment/test",
            "commit": SOURCE_COMMIT,
            "dirty": False,
            "tracked_change_count": 0,
            "untracked_file_count": 0,
        },
        "run": {
            "board_type": "OrangePi-5-Plus",
            "execution_order": run_number,
            "exit_status": 0,
            "profile": "fault-ack-loss",
            "repeat_count": 3,
            "run_id": RUN_ORDER[run_number - 1],
            "run_number": run_number,
        },
        "board": {
            "id": BOARD_ID,
            "type": "OrangePi-5-Plus",
            "hostname": "orangepi5plus",
        },
        "inputs": {
            "rootfs": {
                "path": "tmp/rootfs.img",
                "sha256": ROOTFS_SHA256,
                "size_bytes": 64 * 1024 * 1024,
            }
        },
        "model": {
            "backend": "native",
            "id": "thermal-4x6x1-v1",
        },
        "outputs": {
            "console_log": {
                "path": str(run_dir / "console.log.gz"),
                "sha256": sha256_bytes(console_gzip),
                "size_bytes": len(console_gzip),
            },
            "raw_csv": {
                "path": str(run_dir / "raw.csv.gz"),
                "sha256": sha256_bytes(raw_gzip),
                "size_bytes": len(raw_gzip),
            },
            "summary": {
                "path": str(run_dir / "summary.json"),
                "sha256": sha256_file(run_dir / "summary.json"),
                "size_bytes": (run_dir / "summary.json").stat().st_size,
            },
        },
        "result": {
            "controller_policy": "neural",
            "sample_count": 100,
            "dropped_samples": 0,
            "deadline_misses": 20,
            "successful_marker": True,
            "validated": True,
        },
    }
    write_json(run_dir / "metadata.json", metadata)

    manifest_files = (
        "console.log",
        "console.log.gz",
        "metadata.json",
        "summary.json",
        "raw.csv",
        "raw.csv.gz",
    )
    manifest = "".join(
        f"{sha256_file(run_dir / name)}  {name}\n" for name in manifest_files
    )
    (run_dir / "checksums.sha256").write_text(
        manifest, encoding="utf-8", newline="\n"
    )


def write_campaign(root: Path) -> tuple[Path, Path, Path]:
    preregistration = {
        "schema_version": 1,
        "campaign_id": "formal-test",
        "status": "frozen-before-first-board-capture",
        "platform": {"board_type": "OrangePi-5-Plus"},
        "capture_contract": {
            "runner_profile": "fault-ack-loss",
            "analyzer_profile": "ack-loss",
            "repeat_count": 3,
            "execution_order": RUN_ORDER,
            "controller_policy": "neural",
            "inference_backend": "native",
            "command_count": 100,
            "period_ms": 100,
            "drop_ack_every": 5,
            "configured_ack_losses": 20,
            "expected_retransmissions": 20,
            "expected_recoveries": 20,
            "expected_fresh_applications": 100,
            "expected_duplicate_receives": 20,
            "expected_status_frames": 120,
            "expected_ack_frames": 100,
            "expected_error_frames": 0,
            "expected_protocol_errors": 0,
            "injected_sequences": INJECTED_SEQUENCES,
        },
        "statistics_policy": {
            "latency_summary": [
                "median",
                "IQR",
                "single-run maximum",
                "worst-of-runs",
            ],
            "latency_claim": (
                "descriptive reliability overhead only; this campaign is not an "
                "RT isolation comparison"
            ),
        },
    }
    preregistration_path = root / "campaign-preregistration.json"
    write_json(preregistration_path, preregistration)

    amendment = {
        "schema_version": 1,
        "amendment_id": "campaign-amendment-005-test",
        "preregistration": {
            "path": preregistration_path.name,
            "sha256": sha256_file(preregistration_path),
            "modified": False,
        },
        "correction": {
            "source_commit": SOURCE_COMMIT,
            "worktree_clean": True,
        },
        "artifacts": {"starry_rootfs_sha256": ROOTFS_SHA256},
        "resumed_capture": {
            "result_root": "amendment-005",
            "execution_order": RUN_ORDER,
        },
    }
    amendment_path = root / "campaign-amendment-005-test.json"
    write_json(amendment_path, amendment)

    result_root = root / "amendment-005" / "fault-ack-loss"
    for run_number, run_id in enumerate(RUN_ORDER, start=1):
        write_run(result_root / run_id, run_number)

    final_check = {
        "schema_version": 1,
        "campaign_id": "formal-test",
        "board": {"type": "OrangePi-5-Plus", "hostname": "orangepi5plus"},
        "lease": {
            "allocated_before_ssh": True,
            "released_after_ssh": True,
            "available_after_release": 1,
            "total": 1,
        },
        "linux_root": {
            "source": "/dev/mmcblk1p2",
            "filesystem": "ext4",
            "options": "rw,noatime",
            "read_write": True,
        },
        "probe": {"exit_status": 0, "success_marker": "BOARD_FINAL_LINUX_RW_VERIFIED"},
        "result": "PASS",
    }
    final_check_path = root / "final-board-linux-root-check.json"
    write_json(final_check_path, final_check)
    return result_root, amendment_path, final_check_path


def refresh_run_identity(run_dir: Path) -> None:
    summary_path = run_dir / "summary.json"
    metadata_path = run_dir / "metadata.json"
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    metadata["outputs"]["summary"]["sha256"] = sha256_file(summary_path)
    metadata["outputs"]["summary"]["size_bytes"] = summary_path.stat().st_size
    write_json(metadata_path, metadata)
    manifest_files = [
        "console.log",
        "console.log.gz",
        "metadata.json",
        "summary.json",
        "raw.csv",
        "raw.csv.gz",
    ]
    if (run_dir / "raw-before-reset.csv").is_file():
        manifest_files.extend(
            ("raw-before-reset.csv", "raw-before-reset.csv.gz")
        )
    (run_dir / "checksums.sha256").write_text(
        "".join(
            f"{sha256_file(run_dir / name)}  {name}\n" for name in manifest_files
        ),
        encoding="utf-8",
        newline="\n",
    )


def write_error_campaign(root: Path) -> tuple[Path, Path, Path]:
    ack_result_root, ack_amendment, final_check = write_campaign(root)
    preregistration_path = root / "campaign-preregistration.json"
    preregistration = json.loads(preregistration_path.read_text(encoding="utf-8"))
    preregistration["campaign_id"] = "formal-error-test"
    preregistration["capture_contract"] = {
        "runner_profile": "fault-error",
        "analyzer_profile": "error",
        "repeat_count": 3,
        "execution_order": RUN_ORDER,
        "controller_policy": "neural",
        "inference_backend": "native",
        "controller_fault_profile": "error",
        "command_count": 100,
        "period_ms": 100,
        "drop_ack_every": 0,
        "expected_retransmissions": 0,
        "expected_recoveries": 0,
        "expected_fresh_applications": 100,
        "expected_duplicate_receives": 0,
        "expected_status_frames": 100,
        "expected_ack_frames": 100,
        "expected_error_frames": 5,
        "expected_protocol_errors": 5,
        "normal_control_must_continue_after_faults": True,
        "faults": [
            {
                "kind": kind,
                "sequence": sequence,
                "expected_error_code": error_code,
                "expected_rtos_reason": reason,
            }
            for kind, sequence, error_code, reason in ERROR_FAULTS
        ],
    }
    preregistration["statistics_policy"]["latency_claim"] = (
        "descriptive recovery overhead only; this campaign is not the "
        "preregistered RT isolation comparison"
    )
    write_json(preregistration_path, preregistration)

    ack_amendment.unlink()
    first_amendment = {
        "schema_version": 1,
        "campaign_id": "formal-error-test",
        "amendment": 1,
        "status": "frozen-before-amendment-first-board-capture",
        "preregistration_sha256": sha256_file(preregistration_path),
        "source": {"commit": SOURCE_COMMIT, "worktree_clean": True},
        "artifacts": {"starry_rootfs": {"sha256": ROOTFS_SHA256}},
        "continued_capture_contract": {
            "result_root": "amendment-004",
            "execution_order": RUN_ORDER,
        },
    }
    first_amendment_path = root / "campaign-amendment-001-test.json"
    write_json(first_amendment_path, first_amendment)
    latest_amendment = {
        "schema_version": 1,
        "campaign_id": "formal-error-test",
        "amendment": 2,
        "status": "frozen-before-post-capture-aggregation",
        "preregistration_sha256": sha256_file(preregistration_path),
        "previous_amendment_sha256": sha256_file(first_amendment_path),
        "source": {"commit": SOURCE_COMMIT, "worktree_clean": True},
        "artifacts": {"starry_rootfs": {"sha256": ROOTFS_SHA256}},
        "continued_capture_contract": {
            "result_root": "amendment-005",
            "execution_order": RUN_ORDER,
        },
    }
    latest_amendment_path = root / "campaign-amendment-002-test.json"
    write_json(latest_amendment_path, latest_amendment)

    result_root = ack_result_root.with_name("fault-error")
    ack_result_root.rename(result_root)
    error_evidence = [
        {
            "controller_observed": True,
            "error_code": error_code,
            "kind": kind,
            "reason": reason,
            "rtos_observed": True,
            "sequence": sequence,
        }
        for kind, sequence, error_code, reason in ERROR_FAULTS
    ]
    for run_id in RUN_ORDER:
        run_dir = result_root / run_id
        summary_path = run_dir / "summary.json"
        summary = json.loads(summary_path.read_text(encoding="utf-8"))
        summary["profile"] = "error"
        summary["controller"].update(
            {"retransmissions": 0, "recoveries": 0, "deadline_misses": 0}
        )
        summary["rtos"] = {
            "profile": "error",
            "accepted": 100,
            "applied": 100,
            "duplicates": 0,
            "acks_dropped": 0,
            "status_sent": 100,
            "acks_sent": 100,
            "errors_sent": 5,
            "protocol_errors": 5,
        }
        summary["starry"]["fault_profile"] = "error"
        summary["raw_samples"]["deadline_misses"] = 0
        summary["error_evidence"] = error_evidence
        summary["error_recovery"] = {
            "continued": True,
            "errors_received": 5,
            "injected": 5,
            "normal_acknowledged": 100,
        }
        write_json(summary_path, summary)

        metadata_path = run_dir / "metadata.json"
        metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
        metadata["run"]["profile"] = "fault-error"
        metadata["result"]["deadline_misses"] = 0
        write_json(metadata_path, metadata)
        refresh_run_identity(run_dir)

    final_check_value = json.loads(final_check.read_text(encoding="utf-8"))
    final_check_value["campaign_id"] = "formal-error-test"
    write_json(final_check, final_check_value)
    return result_root, latest_amendment_path, final_check


def write_restart_campaign(root: Path) -> tuple[Path, Path, Path]:
    ack_result_root, amendment_path, final_check = write_campaign(root)
    preregistration_path = root / "campaign-preregistration.json"
    preregistration = json.loads(preregistration_path.read_text(encoding="utf-8"))
    preregistration["campaign_id"] = "formal-restart-test"
    preregistration["capture_contract"] = {
        "runner_profile": "fault-restart",
        "analyzer_profile": "restart",
        "repeat_count": 3,
        "execution_order": RUN_ORDER,
        "controller_policy": "neural",
        "inference_backend": "native",
        "controller_fault_profile": "restart",
        "command_count": 100,
        "pre_reset_command_count": 20,
        "period_ms": 100,
        "expected_retransmissions": 0,
        "expected_recoveries": 0,
        "expected_fresh_applications": 120,
        "expected_duplicate_receives": 0,
        "expected_status_frames": 121,
        "expected_ack_frames": 121,
        "expected_error_frames": 1,
        "expected_protocol_errors": 1,
        "expected_session_resets": 1,
        "expected_session_rejections": 1,
        "expected_safe_fallbacks": 1,
        "expected_endpoint_recoveries": 1,
        "expected_stale_status_frames": 1,
        "expected_stale_ack_frames": 1,
        "expected_retired_control_rejections": 1,
        "restart_vm_id": 1,
        "restart_delay_ms": 20_000,
        "restart_ready_timeout_ms": 30_000,
        "previous_session": 286_331_153,
        "current_session": 572_662_306,
        "actual_vm_reset_required": True,
    }
    preregistration["statistics_policy"]["latency_claim"] = (
        "descriptive restart recovery overhead only; this campaign is not the "
        "preregistered RT isolation comparison"
    )
    write_json(preregistration_path, preregistration)

    amendment = json.loads(amendment_path.read_text(encoding="utf-8"))
    amendment["preregistration"]["sha256"] = sha256_file(preregistration_path)
    write_json(amendment_path, amendment)

    result_root = ack_result_root.with_name("fault-restart")
    ack_result_root.rename(result_root)
    pre_reset_raw = raw_bytes(20)
    pre_reset_gzip = gzip.compress(pre_reset_raw, mtime=0)
    pre_reset_sha256 = sha256_bytes(pre_reset_raw)
    for run_id in RUN_ORDER:
        run_dir = result_root / run_id
        (run_dir / "raw-before-reset.csv").write_bytes(pre_reset_raw)
        (run_dir / "raw-before-reset.csv.gz").write_bytes(pre_reset_gzip)

        summary_path = run_dir / "summary.json"
        summary = json.loads(summary_path.read_text(encoding="utf-8"))
        summary["profile"] = "restart"
        summary["controller"].update(
            {"retransmissions": 0, "recoveries": 0, "deadline_misses": 0}
        )
        summary["rtos"] = {
            "profile": "restart",
            "accepted": 120,
            "applied": 120,
            "duplicates": 0,
            "acks_dropped": 0,
            "status_sent": 121,
            "acks_sent": 121,
            "errors_sent": 1,
            "protocol_errors": 1,
            "session_resets": 1,
            "session_rejections": 1,
            "safe_fallbacks": 1,
            "recoveries": 1,
            "stale_status_sent": 1,
            "stale_acks_sent": 1,
        }
        summary["starry"]["fault_profile"] = "restart"
        summary["raw_samples"]["deadline_misses"] = 0
        summary["pre_reset_raw_samples"] = {
            "path": str(run_dir / "raw-before-reset.csv.gz"),
            "sha256": pre_reset_sha256,
            "artifact_sha256": sha256_bytes(pre_reset_gzip),
            "guest_manifest_sha256": pre_reset_sha256,
            "uart_sha256": pre_reset_sha256,
            "uart_sha256_complete": True,
            "sample_count": 20,
            "dropped_samples": 0,
            "deadline_misses": 0,
            "full_loop_p99_us": 1010,
            "full_loop_max_us": 1010,
        }
        summary["restart_recovery"] = {
            "actual_vm_reset": True,
            "vm_id": 1,
            "reset_count": 1,
            "ready_wait_ms": 450,
            "requested_delay_ms": 20_000,
            "observed_delay_ms": 20_001,
            "old_session": 286_331_153,
            "new_session": 572_662_306,
            "pre_reset_samples": 20,
            "post_reset_samples": 100,
            "safe_fallback_observed": True,
            "recovered": True,
            "stale_ack_ignored": 1,
            "stale_status_ignored": 1,
            "retired_control_rejected": 1,
        }
        write_json(summary_path, summary)

        metadata_path = run_dir / "metadata.json"
        metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
        metadata["run"]["profile"] = "fault-restart"
        metadata["result"]["deadline_misses"] = 0
        write_json(metadata_path, metadata)
        refresh_run_identity(run_dir)

    final_check_value = json.loads(final_check.read_text(encoding="utf-8"))
    final_check_value["campaign_id"] = "formal-restart-test"
    write_json(final_check, final_check_value)
    return result_root, amendment_path, final_check


class BoardCampaignAggregationTests(unittest.TestCase):
    def test_three_valid_restart_runs_require_actual_reset_and_both_raw_phases(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result_root, amendment, final_check = write_restart_campaign(root)

            result = aggregate.aggregate_campaign(
                root, result_root, amendment, final_check
            )

        self.assertTrue(result["assessment"]["campaign_gate_met"])
        self.assertEqual(result["campaign"]["profile"], "fault-restart")
        self.assertEqual(result["fault_contract"]["analyzer_profile"], "restart")
        self.assertTrue(result["fault_contract"]["actual_vm_reset_required"])
        for run in result["evidence"]["runs"]:
            self.assertEqual(run["pre_reset_raw"]["sample_count"], 20)
            self.assertTrue(run["restart_recovery"]["actual_vm_reset"])

    def test_rejects_rehashed_restart_summary_without_actual_vm_reset(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result_root, amendment, final_check = write_restart_campaign(root)
            run_dir = result_root / "run-002"
            summary_path = run_dir / "summary.json"
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
            summary["restart_recovery"]["actual_vm_reset"] = False
            write_json(summary_path, summary)
            refresh_run_identity(run_dir)

            with self.assertRaisesRegex(
                aggregate.AggregationError, "restart_recovery"
            ):
                aggregate.aggregate_campaign(
                    root, result_root, amendment, final_check
                )

    def test_three_valid_error_runs_meet_exact_error_contract(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result_root, amendment, final_check = write_error_campaign(root)

            result = aggregate.aggregate_campaign(
                root, result_root, amendment, final_check
            )

        self.assertTrue(result["assessment"]["campaign_gate_met"])
        self.assertEqual(result["campaign"]["profile"], "fault-error")
        self.assertEqual(result["fault_contract"]["analyzer_profile"], "error")
        self.assertEqual(result["fault_contract"]["expected_error_frames_per_run"], 5)
        self.assertEqual(result["fault_contract"]["faults"], [
            {
                "error_code": error_code,
                "kind": kind,
                "reason": reason,
                "sequence": sequence,
            }
            for kind, sequence, error_code, reason in ERROR_FAULTS
        ])

    def test_rejects_rehashed_error_evidence_contract_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result_root, amendment, final_check = write_error_campaign(root)
            run_dir = result_root / "run-002"
            summary_path = run_dir / "summary.json"
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
            summary["error_evidence"][0]["error_code"] = 1
            write_json(summary_path, summary)
            refresh_run_identity(run_dir)

            with self.assertRaisesRegex(
                aggregate.AggregationError, "error_evidence"
            ):
                aggregate.aggregate_campaign(
                    root, result_root, amendment, final_check
                )

    def test_three_valid_runs_meet_campaign_gate_with_inclusive_iqr(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result_root, amendment, final_check = write_campaign(root)

            result = aggregate.aggregate_campaign(
                root, result_root, amendment, final_check
            )

        self.assertTrue(result["assessment"]["campaign_gate_met"])
        self.assertEqual(result["campaign"]["run_order"], RUN_ORDER)
        metric = result["latency"]["metrics"]["full_loop_p99_us"]
        self.assertEqual(metric["values_by_run"], [300, 600, 900])
        self.assertEqual(metric["median"], 600)
        self.assertEqual(metric["q1"], 450)
        self.assertEqual(metric["q3"], 750)
        self.assertEqual(metric["iqr"], 300)
        self.assertEqual(metric["single_run_maximum"], 900)
        self.assertEqual(metric["worst_of_runs"], 900)

    def test_rejects_a_tampered_manifest_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result_root, amendment, final_check = write_campaign(root)
            with (result_root / "run-002" / "raw.csv").open("ab") as stream:
                stream.write(b"tampered\n")

            with self.assertRaisesRegex(aggregate.AggregationError, "checksum"):
                aggregate.aggregate_campaign(
                    root, result_root, amendment, final_check
                )

    def test_rejects_a_fault_count_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result_root, amendment, final_check = write_campaign(root)
            summary_path = result_root / "run-003" / "summary.json"
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
            summary["rtos"]["duplicates"] = 19
            write_json(summary_path, summary)
            metadata_path = result_root / "run-003" / "metadata.json"
            metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
            metadata["outputs"]["summary"]["sha256"] = sha256_file(summary_path)
            metadata["outputs"]["summary"]["size_bytes"] = summary_path.stat().st_size
            write_json(metadata_path, metadata)
            manifest_path = result_root / "run-003" / "checksums.sha256"
            manifest_files = (
                "console.log",
                "console.log.gz",
                "metadata.json",
                "summary.json",
                "raw.csv",
                "raw.csv.gz",
            )
            manifest_path.write_text(
                "".join(
                    f"{sha256_file(result_root / 'run-003' / name)}  {name}\n"
                    for name in manifest_files
                ),
                encoding="utf-8",
                newline="\n",
            )

            with self.assertRaisesRegex(aggregate.AggregationError, "duplicates"):
                aggregate.aggregate_campaign(
                    root, result_root, amendment, final_check
                )

    def test_rejects_a_non_rw_final_linux_root(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result_root, amendment, final_check = write_campaign(root)
            check = json.loads(final_check.read_text(encoding="utf-8"))
            check["linux_root"]["read_write"] = False
            check["result"] = "FAIL"
            write_json(final_check, check)

            with self.assertRaisesRegex(aggregate.AggregationError, "read-write"):
                aggregate.aggregate_campaign(
                    root, result_root, amendment, final_check
                )

    def test_cli_writes_platform_independent_lf_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result_root, amendment, final_check = write_campaign(root)
            output = root / "campaign-summary.json"

            status = aggregate.main(
                [
                    str(root),
                    "--result-root",
                    str(result_root),
                    "--latest-amendment",
                    str(amendment),
                    "--final-board-check",
                    str(final_check),
                    "--output",
                    str(output),
                ]
            )

            self.assertEqual(status, 0)
            self.assertNotIn(b"\r\n", output.read_bytes())
            self.assertTrue(
                json.loads(output.read_text(encoding="utf-8"))["assessment"][
                    "campaign_gate_met"
                ]
            )


if __name__ == "__main__":
    unittest.main()
