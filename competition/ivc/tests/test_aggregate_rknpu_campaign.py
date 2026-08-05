from __future__ import annotations

import gzip
import hashlib
import importlib.util
import json
import struct
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path


IVC_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(IVC_DIR))
SPEC = importlib.util.spec_from_file_location(
    "ivc_aggregate_rknpu_campaign",
    IVC_DIR / "aggregate_rknpu_campaign.py",
)
assert SPEC is not None and SPEC.loader is not None
aggregate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = aggregate
SPEC.loader.exec_module(aggregate)


SOURCE_COMMIT = "a" * 40
MODEL_SHA256 = aggregate.DEFAULT_MODEL_SHA256


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def write_json(path: Path, value: object) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def artifact_record(path: str) -> dict[str, object]:
    return {
        "path": path,
        "sha256": hashlib.sha256(path.encode()).hexdigest(),
        "size_bytes": len(path),
    }


def output_record(path: Path, stored_path: str) -> dict[str, object]:
    return {
        "path": stored_path,
        "sha256": sha256_file(path),
        "size_bytes": path.stat().st_size,
    }


def raw_bytes(run_number: int) -> bytes:
    rows = [
        (1, 0, 23_000 + run_number, 140_000 + run_number),
        (
            2,
            140_000 + run_number,
            141_600 + 2 * run_number,
            148_000 + 2 * run_number,
        ),
        (
            3,
            240_000 + run_number,
            241_700 + 2 * run_number,
            249_000 + 2 * run_number,
        ),
        (
            4,
            340_000 + run_number,
            341_800 + 2 * run_number,
            353_000 + 2 * run_number,
        ),
    ]
    lines = [
        "sequence,cycle_started_us,command_sent_us,response_completed_us,full_loop_us,"
        "pre_send_us,transport_us,setpoint_milli_c,observed_milli_c,measured_milli_c,"
        "command_actuator_permille,status_actuator_permille,error_milli_c"
    ]
    measured_milli_c = 20_000
    for sequence, started, sent, completed in rows:
        next_measured_milli_c = measured_milli_c + 200
        actuator_permille = 950 + sequence
        lines.append(
            ",".join(
                str(value)
                for value in (
                    sequence,
                    started,
                    sent,
                    completed,
                    completed - started,
                    sent - started,
                    completed - sent,
                    45_000,
                    measured_milli_c,
                    next_measured_milli_c,
                    actuator_permille,
                    actuator_permille,
                    45_000 - next_measured_milli_c,
                )
            )
        )
        measured_milli_c = next_measured_milli_c
    return ("\n".join(lines) + "\n").encode()


def rknn_bytes(run_number: int) -> bytes:
    lines = [
        "sequence,input0_bits,input1_bits,input2_bits,input3_bits,output_bits,"
        "actuator_permille,wall_ns,device_us"
    ]
    timings = (
        (20_000_000 + run_number, 16_600 + run_number),
        (1_680_000 + run_number, 1_590 + run_number),
        (1_680_000 + run_number, 1_590 + run_number),
        (1_760_000 + run_number, 1_660 + run_number),
    )
    for sequence, (wall_ns, device_us) in enumerate(timings, start=1):
        actuator_permille = 950 + sequence
        output_bits = struct.pack(">f", actuator_permille / 1000).hex()
        lines.append(
            f"{sequence},3f000000,3ea00000,00000000,00000000,{output_bits},"
            f"{actuator_permille},{wall_ns},{device_us}"
        )
    return ("\n".join(lines) + "\n").encode()


def refresh_manifest(run_dir: Path) -> None:
    (run_dir / "checksums.sha256").write_text(
        "".join(
            f"{sha256_file(run_dir / name)}  {name}\n"
            for name in aggregate.MANIFEST_FILES
        ),
        encoding="utf-8",
        newline="\n",
    )


def write_run(
    run_dir: Path,
    run_number: int,
    started_at: datetime,
    marker_copies: int = 3,
    source_commit: str = SOURCE_COMMIT,
) -> None:
    run_dir.mkdir(parents=True, exist_ok=True)
    raw_contents = raw_bytes(run_number)
    rknn_contents = rknn_bytes(run_number)
    raw_digest = sha256_bytes(raw_contents)
    rknn_digest = sha256_bytes(rknn_contents)
    console_text = "".join(
        (
            f"[guest-console:pl011-starry] IVC-STARRY-RKNN-MODEL sha256={MODEL_SHA256}\n"
            f"[guest-console:pl011-starry] IVC-STARRY-RKNN-RAW sha256={rknn_digest}\n"
        )
        for _ in range(marker_copies)
    )
    console_bytes = console_text.encode()
    artifacts = {
        "console.log": console_bytes,
        "console.log.gz": gzip.compress(console_bytes, mtime=0),
        "raw.csv": raw_contents,
        "raw.csv.gz": gzip.compress(raw_contents, mtime=0),
        "rknn.csv": rknn_contents,
        "rknn.csv.gz": gzip.compress(rknn_contents, mtime=0),
        "stage.log": b"stage passed\n",
    }
    for name, contents in artifacts.items():
        (run_dir / name).write_bytes(contents)

    controller = {
        "policy": "neural",
        "sent": 4,
        "acknowledged": 4,
        "errors": 0,
        "timeouts": 0,
        "retransmissions": 0,
        "recoveries": 0,
        "success_percent": 100,
        "deadline_misses": 1,
        "full_loop_p50_us": 9_000 + run_number,
        "full_loop_p95_us": 13_000 + run_number,
        "full_loop_p99_us": 13_000 + run_number,
        "full_loop_max_us": 140_000 + run_number,
        "pre_send_p50_us": 1_700 + run_number,
        "pre_send_p95_us": 1_800 + run_number,
        "pre_send_p99_us": 1_800 + run_number,
        "pre_send_max_us": 23_000 + run_number,
        "transport_p50_us": 7_300,
        "transport_p95_us": 11_200,
        "transport_p99_us": 11_200,
        "transport_max_us": 117_000,
        "throughput_msg_s": 9.99,
    }
    rknn = {
        "sample_count": 4,
        "positive_device_times": 4,
        "actuator_matches": 4,
        "core_mask": 0,
        "runtime_api": "2.3.2",
        "runtime_driver": "0.9.8",
        "initialization_us": 77_000 + run_number,
        "model_sha256": MODEL_SHA256,
        "sha256": rknn_digest,
        "guest_manifest_sha256": rknn_digest,
        "artifact_sha256": sha256_file(run_dir / "rknn.csv.gz"),
        "device_p50_us": 1_590 + run_number,
        "device_p99_us": 1_660 + run_number,
        "device_max_us": 16_600 + run_number,
        "wall_p50_ns": 1_680_000 + run_number,
        "wall_p99_ns": 1_760_000 + run_number,
        "wall_max_ns": 20_000_000 + run_number,
    }
    summary = {
        "schema_version": 2,
        "platform": "orangepi-5-plus",
        "guest": "starryos",
        "profile": "normal",
        "starry": {
            "mode": "neural",
            "backend": "rknn-npu",
            "fault_profile": "none",
            "count": 4,
            "period_ms": 100,
            "vcpus": 2,
        },
        "controller": controller,
        "rtos": {
            "accepted": 4,
            "applied": 4,
            "status_sent": 4,
            "acks_sent": 4,
            "duplicates": 0,
            "acks_dropped": 0,
            "errors_sent": 0,
            "protocol_errors": 0,
        },
        "lifecycle": {
            "board_linux_restored": True,
            "host_filesystem_synced": True,
            "rtos_powered_off": True,
            "starry_done": True,
            "volatile_block_snapshotted": True,
            "block_snapshot": {
                "filesystem_check": "clean",
                "image_sha256": "b" * 64,
            },
        },
        "raw_samples": {
            "sample_count": 4,
            "dropped_samples": 0,
            "sha256": raw_digest,
            "guest_manifest_sha256": raw_digest,
            "uart_sha256": raw_digest,
            "uart_sha256_complete": True,
            "artifact_sha256": sha256_file(run_dir / "raw.csv.gz"),
        },
        "rknn_samples": rknn,
        "source_log": {
            "content_sha256": sha256_file(run_dir / "console.log"),
            "sha256": sha256_file(run_dir / "console.log.gz"),
        },
    }
    write_json(run_dir / "summary.json", summary)

    finished_at = started_at + timedelta(minutes=7)
    inputs = {
        name: artifact_record(f"artifacts/{name}")
        for name in (
            "board_config",
            "build_config",
            "rootfs",
            "starry_dtb",
            "starry_kernel",
            "zephyr_guest",
        )
    }
    model_artifact = artifact_record("artifacts/model.rknn")
    model_artifact["sha256"] = MODEL_SHA256
    metadata = {
        "schema_version": 1,
        "source": {
            "branch": "experiment/test",
            "commit": source_commit,
            "dirty": False,
            "tracked_change_count": 0,
            "untracked_file_count": 0,
        },
        "run": {
            "run_id": run_dir.name,
            "run_number": run_number,
            "execution_order": run_number,
            "repeat_count": 5,
            "profile": "rknpu-full",
            "exit_status": 0,
            "started_at": started_at.isoformat().replace("+00:00", "Z"),
            "finished_at": finished_at.isoformat().replace("+00:00", "Z"),
        },
        "result": {
            "validated": True,
            "successful_marker": True,
            "controller_policy": "neural",
            "sample_count": 4,
            "rknn_sample_count": 4,
            "dropped_samples": 0,
        },
        "model": {
            "id": "thermal-4x6x1-v1",
            "backend": "rknn-npu",
            "runtime_version": "2.3.2",
            "artifact": model_artifact,
        },
        "inputs": inputs,
        "outputs": {
            "console_log": output_record(
                run_dir / "console.log.gz", f"results/{run_dir.name}/console.log.gz"
            ),
            "raw_csv": output_record(
                run_dir / "raw.csv.gz", f"results/{run_dir.name}/raw.csv.gz"
            ),
            "rknn_csv": output_record(
                run_dir / "rknn.csv.gz", f"results/{run_dir.name}/rknn.csv.gz"
            ),
            "summary": output_record(
                run_dir / "summary.json", f"results/{run_dir.name}/summary.json"
            ),
        },
        "board": {
            "id": "0123456789abcdef",
            "hostname": "orangepi5plus",
            "cpu_temp_milli_c": 42_000 + run_number,
        },
    }
    write_json(run_dir / "metadata.json", metadata)
    refresh_manifest(run_dir)


class RknpuCampaignAggregationTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        started = datetime(2026, 8, 5, tzinfo=timezone.utc)
        for run_number in range(1, 6):
            write_run(
                self.root / f"run-{run_number:03d}",
                run_number,
                started + timedelta(minutes=8 * (run_number - 1)),
            )

    def test_valid_campaign_is_aggregated(self) -> None:
        result = aggregate.aggregate_campaign(
            self.root,
            expected_runs=5,
            expected_count=4,
            expected_commit=SOURCE_COMMIT,
        )

        self.assertEqual(result["campaign"]["total_samples"], 20)
        self.assertEqual(result["reliability"]["acknowledged"], 20)
        self.assertEqual(result["reliability"]["deadline_misses"], 5)
        self.assertEqual(
            result["statistics"]["controller"]["full_loop_p99_us"]["median"],
            13_003,
        )
        self.assertEqual(result["deadline_partition"]["first_cycle"]["deadline_misses"], 5)
        self.assertEqual(
            result["deadline_partition"]["post_first_cycle"]["deadline_misses"],
            0,
        )
        self.assertEqual(len(result["runs"]), 5)

    def test_dirty_run_is_rejected(self) -> None:
        run_dir = self.root / "run-003"
        metadata = json.loads((run_dir / "metadata.json").read_text())
        metadata["source"]["dirty"] = True
        write_json(run_dir / "metadata.json", metadata)
        refresh_manifest(run_dir)

        with self.assertRaisesRegex(aggregate.AggregationError, "dirty"):
            aggregate.aggregate_campaign(self.root, expected_count=4)

    def test_tampered_artifact_is_rejected_by_manifest(self) -> None:
        (self.root / "run-002" / "rknn.csv").write_bytes(b"tampered\n")

        with self.assertRaisesRegex(aggregate.AggregationError, "checksum mismatch"):
            aggregate.aggregate_campaign(self.root, expected_count=4)

    def test_mixed_source_commits_are_rejected(self) -> None:
        run_dir = self.root / "run-004"
        metadata = json.loads((run_dir / "metadata.json").read_text())
        metadata["source"]["commit"] = "c" * 40
        write_json(run_dir / "metadata.json", metadata)
        refresh_manifest(run_dir)

        with self.assertRaisesRegex(aggregate.AggregationError, "multiple commits"):
            aggregate.aggregate_campaign(self.root, expected_count=4)

    def test_raw_deadline_recomputation_rejects_summary_mismatch(self) -> None:
        run_dir = self.root / "run-001"
        summary = json.loads((run_dir / "summary.json").read_text())
        controller = dict(summary["controller"])
        controller["deadline_misses"] = 0

        with self.assertRaisesRegex(
            aggregate.rknpu_deadline.DeadlineAnalysisError,
            "deadline misses do not match raw CSV",
        ):
            aggregate.rknpu_deadline.analyze_run(
                run_dir / "raw.csv",
                run_dir / "rknn.csv",
                4,
                controller,
                summary["rknn_samples"],
            )

    def test_single_compact_hash_marker_is_rejected(self) -> None:
        run_dir = self.root / "run-003"
        for path in run_dir.iterdir():
            path.unlink()
        write_run(
            run_dir,
            3,
            datetime(2026, 8, 5, 0, 16, tzinfo=timezone.utc),
            marker_copies=1,
        )

        with self.assertRaisesRegex(aggregate.AggregationError, "two-record UART"):
            aggregate.aggregate_campaign(self.root, expected_count=4)


if __name__ == "__main__":
    unittest.main()
