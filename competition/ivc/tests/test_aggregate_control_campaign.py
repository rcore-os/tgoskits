from __future__ import annotations

import gzip
import hashlib
import importlib.util
import json
import math
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path


IVC_DIR = Path(__file__).resolve().parents[1]
if str(IVC_DIR) not in sys.path:
    sys.path.insert(0, str(IVC_DIR))
SPEC = importlib.util.spec_from_file_location(
    "ivc_aggregate_control_campaign",
    IVC_DIR / "aggregate_control_campaign.py",
)
assert SPEC is not None and SPEC.loader is not None
aggregate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = aggregate
SPEC.loader.exec_module(aggregate)


SOURCE_COMMIT = "a" * 40
BOARD_ID = "0123456789abcdef"
NETWORK = {
    "iface": "eth0",
    "ip": "10.0.0.1/24",
    "peer": "10.0.0.2",
    "udp_port": 5500,
    "segment": 1,
}
RAW_HEADER = (
    "sequence,cycle_started_us,command_sent_us,response_completed_us,"
    "full_loop_us,pre_send_us,transport_us,setpoint_milli_c,"
    "observed_milli_c,measured_milli_c,command_actuator_permille,"
    "status_actuator_permille,error_milli_c\n"
)
PROFILE_CONTRACTS = {
    "manual": {
        "runner_profile": "manual-full",
        "controller_policy": "manual-fixed",
        "starry_mode": "manual",
        "model_id": "manual-fixed-500",
        "result_snapshot_path": "/home/orangepi/ivc-m",
        "build_config": "competition/ivc/config/axvisor-orangepi-5-plus-manual.toml",
        "board_config": "competition/ivc/config/board-orangepi-5-plus-manual.toml",
        "rootfs": "tmp/competition/ivc/starry/starry-ivc-rootfs-manual.img",
    },
    "neural": {
        "runner_profile": "full",
        "controller_policy": "neural",
        "starry_mode": "neural",
        "model_id": "thermal-4x6x1-v1",
        "result_snapshot_path": "/home/orangepi/ivc-n",
        "build_config": "competition/ivc/config/axvisor-orangepi-5-plus.toml",
        "board_config": "competition/ivc/config/board-orangepi-5-plus.toml",
        "rootfs": "tmp/competition/ivc/starry/starry-ivc-rootfs.img",
    },
}
SHARED_ARTIFACT_PATHS = {
    "starry_kernel": "tmp/competition/ivc/starry/starryos.bin",
    "starry_dtb": "tmp/competition/ivc/starry/starry-orangepi-5-plus.dtb",
    "zephyr_guest": "competition/ivc/zephyr/build-board/zephyr/zephyr.bin",
    "model_source": "tools/ivcproto/src/neural.rs",
}


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


def contract_record(path: str) -> dict[str, object]:
    return {
        "path": path,
        "sha256": hashlib.sha256(path.encode()).hexdigest(),
        "size_bytes": len(path),
    }


def profile_input_records(profile_name: str) -> dict[str, dict[str, object]]:
    contract = PROFILE_CONTRACTS[profile_name]
    return {
        name: contract_record(str(contract[name]))
        for name in ("build_config", "board_config", "rootfs")
    }


def shared_artifact_records() -> dict[str, dict[str, object]]:
    return {
        name: contract_record(path)
        for name, path in SHARED_ARTIFACT_PATHS.items()
    }


def control_raw(profile_name: str) -> tuple[bytes, list[int], int]:
    rows = [RAW_HEADER]
    errors: list[int] = []
    full_loop_us = 2_000 if profile_name == "manual" else 1_500
    previous_measured = 0
    for sequence in range(1, 1801):
        segment_offset = (sequence - 1) % 600
        if profile_name == "manual":
            error = 2_000
        elif segment_offset < 10:
            error = 2_000
        elif segment_offset == 10:
            error = -1_500
        else:
            error = 500
        setpoint = 45_000 if sequence <= 600 else 65_000 if sequence <= 1200 else 50_000
        measured = setpoint - error
        cycle = sequence * 100_000
        errors.append(error)
        rows.append(
            f"{sequence},{cycle},{cycle + 10},{cycle + full_loop_us},"
            f"{full_loop_us},10,{full_loop_us - 10},{setpoint},{previous_measured},"
            f"{measured},500,500,{error}\n"
        )
        previous_measured = measured
    return "".join(rows).encode(), errors, full_loop_us


def refresh_manifest(run_dir: Path) -> None:
    names = (
        "console.log",
        "console.log.gz",
        "metadata.json",
        "summary.json",
        "raw.csv",
        "raw.csv.gz",
    )
    (run_dir / "checksums.sha256").write_text(
        "".join(f"{sha256_file(run_dir / name)}  {name}\n" for name in names),
        encoding="utf-8",
        newline="\n",
    )


def write_run(
    run_dir: Path,
    profile_name: str,
    started_at: datetime,
    finished_at: datetime,
    cpu_temp_milli_c: int,
) -> None:
    run_dir.mkdir(parents=True)
    raw, errors, full_loop_us = control_raw(profile_name)
    console = f"formal {profile_name} StarryOS control run\n".encode()
    raw_gzip = gzip.compress(raw, mtime=0)
    console_gzip = gzip.compress(console, mtime=0)
    (run_dir / "raw.csv").write_bytes(raw)
    (run_dir / "raw.csv.gz").write_bytes(raw_gzip)
    (run_dir / "console.log").write_bytes(console)
    (run_dir / "console.log.gz").write_bytes(console_gzip)

    contract = PROFILE_CONTRACTS[profile_name]
    raw_sha256 = sha256_bytes(raw)
    rmse = math.sqrt(sum(error * error for error in errors) / len(errors))
    iae = sum(abs(error) for error in errors) / 10
    max_overshoot = max(0, max(-error for error in errors))
    summary = {
        "schema_version": 2,
        "platform": "orangepi-5-plus",
        "guest": "starryos",
        "profile": "normal",
        "board": {
            "board_id": BOARD_ID,
            "hostname": "orangepi5plus",
            "cpu_temp_milli_c": cpu_temp_milli_c,
        },
        "controller": {
            "policy": contract["controller_policy"],
            "sent": 1800,
            "acknowledged": 1800,
            "errors": 0,
            "timeouts": 0,
            "retransmissions": 0,
            "recoveries": 0,
            "deadline_misses": 0,
            "success_percent": 100.0,
            "full_loop_p50_us": full_loop_us,
            "full_loop_p95_us": full_loop_us,
            "full_loop_p99_us": full_loop_us,
            "full_loop_max_us": full_loop_us,
            "pre_send_p50_us": 10,
            "pre_send_p95_us": 10,
            "pre_send_p99_us": 10,
            "pre_send_max_us": 10,
            "transport_p50_us": full_loop_us - 10,
            "transport_p95_us": full_loop_us - 10,
            "transport_p99_us": full_loop_us - 10,
            "transport_max_us": full_loop_us - 10,
            "throughput_msg_s": 10.0,
            "rmse_milli_c": rmse,
            "iae_milli_c_s": iae,
            "max_overshoot_milli_c": max_overshoot,
        },
        "rtos": {
            "profile": "normal",
            "accepted": 1800,
            "applied": 1800,
            "duplicates": 0,
            "acks_dropped": 0,
            "status_sent": 1800,
            "acks_sent": 1800,
            "errors_sent": 0,
            "protocol_errors": 0,
        },
        "starry": {
            "mode": contract["starry_mode"],
            "backend": "native",
            "fault_profile": "none",
            "count": 1800,
            "period_ms": 100,
            "vcpus": 2,
        },
        "network": NETWORK,
        "lifecycle": {
            "starry_done": True,
            "rtos_powered_off": True,
            "host_filesystem_synced": True,
            "volatile_block_snapshotted": True,
            "board_linux_restored": True,
            "block_snapshot": {
                "filesystem_check": "clean",
                "image_path": contract["result_snapshot_path"],
                "vm_id": 1,
            },
        },
        "raw_samples": {
            "path": str(run_dir / "raw.csv.gz"),
            "sha256": raw_sha256,
            "uart_sha256": raw_sha256[:12],
            "uart_sha256_complete": False,
            "artifact_sha256": sha256_bytes(raw_gzip),
            "guest_manifest_sha256": raw_sha256,
            "sample_count": 1800,
            "dropped_samples": 0,
            "deadline_misses": 0,
        },
        "source_log": {
            "path": str(run_dir / "console.log.gz"),
            "sha256": sha256_bytes(console_gzip),
            "content_sha256": sha256_bytes(console),
        },
    }
    write_json(run_dir / "summary.json", summary)

    shared = shared_artifact_records()
    metadata = {
        "schema_version": 1,
        "source": {
            "branch": "experiment/control-test",
            "commit": SOURCE_COMMIT,
            "dirty": False,
            "tracked_change_count": 0,
            "untracked_file_count": 0,
        },
        "run": {
            "profile": contract["runner_profile"],
            "run_id": "run-001",
            "run_number": 1,
            "repeat_count": 1,
            "execution_order": 1,
            "board_type": "OrangePi-5-Plus",
            "started_at": started_at.isoformat().replace("+00:00", "Z"),
            "finished_at": finished_at.isoformat().replace("+00:00", "Z"),
            "exit_status": 0,
        },
        "board": {
            "type": "OrangePi-5-Plus",
            "id": BOARD_ID,
            "hostname": "orangepi5plus",
        },
        "inputs": {
            **profile_input_records(profile_name),
            "starry_kernel": shared["starry_kernel"],
            "starry_dtb": shared["starry_dtb"],
            "zephyr_guest": shared["zephyr_guest"],
        },
        "model": {
            "id": contract["model_id"],
            "backend": "native",
            "runtime_version": "native",
            "artifact": shared["model_source"],
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
            "validated": True,
            "controller_policy": contract["controller_policy"],
            "sample_count": 1800,
            "dropped_samples": 0,
            "deadline_misses": 0,
            "successful_marker": True,
        },
    }
    write_json(run_dir / "metadata.json", metadata)
    refresh_manifest(run_dir)


def write_campaign(root: Path) -> tuple[Path, Path]:
    shared = shared_artifact_records()
    preregistration = {
        "schema_version": 1,
        "campaign_id": "formal-control-test",
        "status": "frozen-before-first-board-capture",
        "platform": {"board_type": "OrangePi-5-Plus"},
        "source": {"commit": SOURCE_COMMIT, "worktree_clean": True},
        "capture_contract": {
            "pair_count": 5,
            "analyzer_profile": "normal",
            "repeat_count_per_half": 1,
            "command_count": 1800,
            "period_ms": 100,
            "expected_retransmissions": 0,
            "expected_recoveries": 0,
            "expected_duplicates": 0,
            "expected_errors": 0,
            "expected_protocol_errors": 0,
            "settling_band_milli_c": 1000,
            "settling_minimum_samples": 20,
            "execution_order": aggregate.PAIR_SCHEDULE,
            "setpoint_schedule": [
                {"start_sequence": 1, "end_sequence": 600, "setpoint_milli_c": 45000},
                {"start_sequence": 601, "end_sequence": 1200, "setpoint_milli_c": 65000},
                {"start_sequence": 1201, "end_sequence": 1800, "setpoint_milli_c": 50000},
            ],
            "network": NETWORK,
            "profiles": {
                name: {
                    "runner_profile": contract["runner_profile"],
                    "controller_policy": contract["controller_policy"],
                    "starry_mode": contract["starry_mode"],
                    "model_id": contract["model_id"],
                    "inference_backend": "native",
                    "result_snapshot_path": contract["result_snapshot_path"],
                    "inputs": profile_input_records(name),
                }
                for name, contract in PROFILE_CONTRACTS.items()
            },
        },
        "artifacts": shared,
        "statistics_policy": {
            "summary": [
                "paired values",
                "median",
                "IQR",
                "single-run maximum",
                "worst-of-runs",
            ]
        },
    }
    write_json(root / "campaign-preregistration.json", preregistration)

    result_root = root / "capture-001"
    cursor = datetime(2026, 8, 4, tzinfo=timezone.utc)
    for pair in aggregate.PAIR_SCHEDULE:
        pair_id = pair["pair_id"]
        for profile_name in pair["order"]:
            started_at = cursor
            finished_at = started_at + timedelta(minutes=3)
            runner_profile = PROFILE_CONTRACTS[profile_name]["runner_profile"]
            write_run(
                result_root / pair_id / str(runner_profile) / "run-001",
                str(profile_name),
                started_at,
                finished_at,
                42_000 + len(list(result_root.rglob("metadata.json"))),
            )
            cursor = finished_at + timedelta(minutes=1)

    final_check = {
        "schema_version": 1,
        "campaign_id": "formal-control-test",
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
        "probe": {
            "exit_status": 0,
            "success_marker": "BOARD_FINAL_LINUX_RW_VERIFIED",
        },
        "result": "PASS",
    }
    final_check_path = root / "final-board-linux-root-check.json"
    write_json(final_check_path, final_check)
    return result_root, final_check_path


class ControlCampaignAggregationTests(unittest.TestCase):
    def test_valid_campaign_reports_gains_and_overshoot_regression(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result_root, final_check = write_campaign(root)

            result = aggregate.aggregate_campaign(root, result_root, final_check)

            self.assertTrue(result["assessment"]["campaign_gate_met"])
            self.assertEqual(result["campaign"]["pair_count"], 5)
            self.assertEqual(
                result["campaign"]["execution_order"], aggregate.PAIR_SCHEDULE
            )
            self.assertTrue(
                result["paired_effects"]["rmse_milli_c"]["all_pairs_favor_neural"]
            )
            self.assertTrue(
                result["paired_effects"]["iae_milli_c_s"]["all_pairs_favor_neural"]
            )
            self.assertFalse(
                result["paired_effects"]["max_overshoot_milli_c"][
                    "all_pairs_favor_neural"
                ]
            )
            first_pair = result["evidence"]["pairs"][0]["profiles"]
            self.assertEqual(first_pair["manual"]["settled_step_count"], 0)
            self.assertEqual(first_pair["neural"]["settled_step_count"], 3)

    def test_rejects_zephyr_guest_hash_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result_root, final_check = write_campaign(root)
            run_dir = result_root / "pair-003" / "manual-full" / "run-001"
            metadata_path = run_dir / "metadata.json"
            metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
            metadata["inputs"]["zephyr_guest"]["sha256"] = "f" * 64
            write_json(metadata_path, metadata)
            refresh_manifest(run_dir)

            with self.assertRaisesRegex(aggregate.AggregationError, "zephyr_guest"):
                aggregate.aggregate_campaign(root, result_root, final_check)

    def test_rejects_dirty_run_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result_root, final_check = write_campaign(root)
            run_dir = result_root / "pair-001" / "manual-full" / "run-001"
            metadata_path = run_dir / "metadata.json"
            metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
            metadata["source"]["dirty"] = True
            metadata["source"]["tracked_change_count"] = 1
            write_json(metadata_path, metadata)
            refresh_manifest(run_dir)

            with self.assertRaisesRegex(aggregate.AggregationError, "dirty"):
                aggregate.aggregate_campaign(root, result_root, final_check)

    def test_rejects_timestamp_order_violation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result_root, final_check = write_campaign(root)
            run_dir = result_root / "pair-002" / "full" / "run-001"
            metadata_path = run_dir / "metadata.json"
            metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
            metadata["run"]["finished_at"] = "2026-08-04T00:30:00Z"
            write_json(metadata_path, metadata)
            refresh_manifest(run_dir)

            with self.assertRaisesRegex(
                aggregate.AggregationError, "timestamps violate the frozen order"
            ):
                aggregate.aggregate_campaign(root, result_root, final_check)


class ControlCampaignRunnerTests(unittest.TestCase):
    def test_formal_dry_run_uses_frozen_ab_ba_schedule(self) -> None:
        commit = subprocess.run(
            ["git", "-C", str(IVC_DIR.parents[1]), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        with tempfile.TemporaryDirectory() as temporary:
            result_root = Path(temporary) / "new-control-campaign"
            environment = os.environ.copy()
            environment["IVC_CONTROL_PROFILE_RUNNER"] = "/bin/true"
            completed = subprocess.run(
                [
                    "bash",
                    str(IVC_DIR / "run-control-campaign.sh"),
                    "formal",
                    "--result-dir",
                    str(result_root),
                    "--expected-commit",
                    commit,
                    "--dry-run",
                ],
                check=True,
                capture_output=True,
                text=True,
                env=environment,
            )

        observed = [
            line.split("profile=", 1)[1]
            for line in completed.stdout.splitlines()
            if line.startswith("CONTROL_CAMPAIGN_HALF")
        ]
        self.assertEqual(
            observed,
            [
                "manual-full",
                "full",
                "full",
                "manual-full",
                "manual-full",
                "full",
                "full",
                "manual-full",
                "manual-full",
                "full",
            ],
        )

    def test_runner_refuses_to_reuse_result_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            environment = os.environ.copy()
            environment["IVC_CONTROL_PROFILE_RUNNER"] = "/bin/true"
            completed = subprocess.run(
                [
                    "bash",
                    str(IVC_DIR / "run-control-campaign.sh"),
                    "smoke",
                    "--result-dir",
                    temporary,
                    "--dry-run",
                ],
                check=False,
                capture_output=True,
                text=True,
                env=environment,
            )

        self.assertEqual(completed.returncode, 73)
        self.assertIn("Refusing to reuse", completed.stderr)


if __name__ == "__main__":
    unittest.main()
