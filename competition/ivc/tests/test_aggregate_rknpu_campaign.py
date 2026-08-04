from __future__ import annotations

import gzip
import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path


IVC_DIR = Path(__file__).resolve().parents[1]
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
RAW_BYTES = b"header\nraw\n"
RKNN_BYTES = b"header\nrknn\n"


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
    raw_digest = sha256_bytes(RAW_BYTES)
    rknn_digest = sha256_bytes(RKNN_BYTES)
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
        "raw.csv": RAW_BYTES,
        "raw.csv.gz": gzip.compress(RAW_BYTES, mtime=0),
        "rknn.csv": RKNN_BYTES,
        "rknn.csv.gz": gzip.compress(RKNN_BYTES, mtime=0),
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
        "full_loop_p50_us": 8_000 + run_number,
        "full_loop_p95_us": 12_000 + run_number,
        "full_loop_p99_us": 13_000 + run_number,
        "full_loop_max_us": 140_000 + run_number,
        "pre_send_p50_us": 1_500 + run_number,
        "pre_send_p95_us": 1_600 + run_number,
        "pre_send_p99_us": 1_700 + run_number,
        "pre_send_max_us": 20_000 + run_number,
        "transport_p50_us": 6_000 + run_number,
        "transport_p95_us": 10_000 + run_number,
        "transport_p99_us": 11_000 + run_number,
        "transport_max_us": 120_000 + run_number,
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
