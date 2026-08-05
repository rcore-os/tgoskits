from __future__ import annotations

import gzip
import hashlib
import importlib.util
import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path


IVC_DIR = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = IVC_DIR.parents[1]
sys.path.insert(0, str(IVC_DIR))
SPEC = importlib.util.spec_from_file_location(
    "ivc_aggregate_ort_campaign",
    IVC_DIR / "aggregate_ort_campaign.py",
)
assert SPEC is not None and SPEC.loader is not None
aggregate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = aggregate
SPEC.loader.exec_module(aggregate)
contract = aggregate.campaign_contract


SOURCE_COMMIT = "a" * 40


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


def output_record(path: Path, stored_path: str) -> dict[str, object]:
    return {
        "path": stored_path,
        "sha256": sha256_file(path),
        "size_bytes": path.stat().st_size,
    }


def raw_bytes(run_number: int) -> bytes:
    timings = (
        (0, 20_000 + run_number, 100_001),
        (100_100, 100_200 + run_number, 110_000 + run_number),
        (200_100, 200_300 + run_number, 212_000 + run_number),
        (300_100, 300_400 + run_number, 313_000 + run_number),
    )
    lines = [
        "sequence,cycle_started_us,command_sent_us,response_completed_us,full_loop_us,"
        "pre_send_us,transport_us,setpoint_milli_c,observed_milli_c,measured_milli_c,"
        "command_actuator_permille,status_actuator_permille,error_milli_c"
    ]
    for sequence, (started, sent, completed) in enumerate(timings, start=1):
        actuator = 950 + sequence
        measured = 20_000 + sequence * 200
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
                    measured - 200,
                    measured,
                    actuator,
                    actuator,
                    45_000 - measured,
                )
            )
        )
    return ("\n".join(lines) + "\n").encode()


def ort_bytes(run_number: int) -> bytes:
    wall_times = (17_000_000 + run_number, 150_000, 160_000, 170_000)
    lines = [
        "sequence,input0_bits,input1_bits,input2_bits,input3_bits,output_bits,"
        "actuator_permille,wall_ns"
    ]
    for sequence, wall_ns in enumerate(wall_times, start=1):
        actuator = 950 + sequence
        output_bits = struct.pack(">f", actuator / 1000.0).hex()
        lines.append(
            f"{sequence},3f000000,3ea00000,00000000,00000000,"
            f"{output_bits},{actuator},{wall_ns}"
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
    preregistration: dict[str, object],
) -> None:
    run_dir.mkdir(parents=True)
    raw_contents = raw_bytes(run_number)
    ort_contents = ort_bytes(run_number)
    raw_digest = sha256_bytes(raw_contents)
    ort_digest = sha256_bytes(ort_contents)
    console = (
        (
            f"IVC-STARRY-ORT-MODEL sha256={contract.DEFAULT_MODEL_SHA256}\n"
            f"IVC-STARRY-ORT-RAW sha256={ort_digest}\n"
        )
        * 2
    ).encode()
    for name, contents in {
        "console.log": console,
        "console.log.gz": gzip.compress(console, mtime=0),
        "raw.csv": raw_contents,
        "raw.csv.gz": gzip.compress(raw_contents, mtime=0),
        "ort.csv": ort_contents,
        "ort.csv.gz": gzip.compress(ort_contents, mtime=0),
        "stage.log": b"stage passed\n",
    }.items():
        (run_dir / name).write_bytes(contents)

    raw_rows = aggregate.board_analysis.read_raw_rows(run_dir / "raw.csv", 4)
    ort_rows = aggregate.board_analysis.read_ort_rows(run_dir / "ort.csv", 4)
    controller = aggregate.board_analysis.derive_raw_metrics(raw_rows, 100)
    controller.update(
        {
            "policy": "neural",
            "sent": 4,
            "acknowledged": 4,
            "errors": 0,
            "timeouts": 0,
            "retransmissions": 0,
            "recoveries": 0,
            "success_percent": 100.0,
        }
    )
    wall_times = sorted(row["wall_ns"] for row in ort_rows)
    summary = {
        "schema_version": 2,
        "platform": "orangepi-5-plus",
        "guest": "starryos",
        "profile": "normal",
        "starry": {
            "mode": "neural",
            "backend": "onnxruntime",
            "fault_profile": "none",
            "count": 4,
            "period_ms": 100,
            "vcpus": 2,
        },
        "controller": controller,
        "rtos": {
            "profile": "normal",
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
        "ort_samples": {
            "sample_count": 4,
            "actuator_matches": 4,
            "runtime_version": contract.DEFAULT_RUNTIME_VERSION,
            "provider": contract.EXPECTED_PROVIDER,
            "initialization_us": 200_000 + run_number,
            "model_sha256": contract.DEFAULT_MODEL_SHA256,
            "sha256": ort_digest,
            "guest_manifest_sha256": ort_digest,
            "artifact_sha256": sha256_file(run_dir / "ort.csv.gz"),
            "wall_p50_ns": aggregate.board_analysis.percentile(wall_times, 50),
            "wall_p99_ns": aggregate.board_analysis.percentile(wall_times, 99),
            "wall_max_ns": wall_times[-1],
        },
    }
    write_json(run_dir / "summary.json", summary)

    model = dict(preregistration["model"])
    model.pop("provider")
    finished_at = started_at + timedelta(minutes=4)
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
            "run_id": run_dir.name,
            "run_number": run_number,
            "execution_order": run_number,
            "repeat_count": 5,
            "profile": "ort-full",
            "exit_status": 0,
            "started_at": started_at.isoformat().replace("+00:00", "Z"),
            "finished_at": finished_at.isoformat().replace("+00:00", "Z"),
        },
        "result": {
            "validated": True,
            "successful_marker": True,
            "controller_policy": "neural",
            "sample_count": 4,
            "ort_sample_count": 4,
            "dropped_samples": 0,
        },
        "model": model,
        "inputs": preregistration["inputs"],
        "outputs": {
            "console_log": output_record(run_dir / "console.log.gz", "console.log.gz"),
            "raw_csv": output_record(run_dir / "raw.csv.gz", "raw.csv.gz"),
            "ort_csv": output_record(run_dir / "ort.csv.gz", "ort.csv.gz"),
            "summary": output_record(run_dir / "summary.json", "summary.json"),
        },
        "board": {
            "id": "0123456789abcdef",
            "hostname": "orangepi5plus",
            "cpu_temp_milli_c": 42_000 + run_number,
        },
    }
    write_json(run_dir / "metadata.json", metadata)
    refresh_manifest(run_dir)


def write_campaign(root: Path) -> Path:
    campaign_root = root / "ort-full"
    campaign_root.mkdir(parents=True)
    artifact_root = root / "artifacts"
    artifact_root.mkdir()
    inputs: dict[str, Path] = {}
    for name in contract.INPUT_NAMES:
        path = artifact_root / name
        path.write_bytes((name + "\n").encode())
        inputs[name] = path
    model_path = IVC_DIR / "model/thermal-4x6x1-v1.ort"
    preregistration = contract.build_preregistration(
        root,
        SOURCE_COMMIT,
        "experiment/test",
        inputs,
        model_path,
        created_at=datetime(2026, 8, 5, tzinfo=timezone.utc),
        run_count=5,
        samples_per_run=4,
    )
    write_json(campaign_root / "preregistration.json", preregistration)
    (campaign_root / "preregistration.sha256").write_text(
        f"{sha256_file(campaign_root / 'preregistration.json')}  preregistration.json\n",
        encoding="utf-8",
        newline="\n",
    )
    started = datetime(2026, 8, 5, 0, 1, tzinfo=timezone.utc)
    for run_number in range(1, 6):
        write_run(
            campaign_root / f"run-{run_number:03d}",
            run_number,
            started + timedelta(minutes=5 * (run_number - 1)),
            preregistration,
        )
    return campaign_root


class OrtCampaignAggregationTests(unittest.TestCase):
    def test_valid_campaign_is_aggregated(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign_root = write_campaign(Path(temporary))
            result = aggregate.aggregate_campaign(
                campaign_root,
                SOURCE_COMMIT,
                expected_runs=5,
                expected_count=4,
            )

        self.assertTrue(result["formal_gate_passed"])
        self.assertEqual(result["campaign"]["total_samples"], 20)
        self.assertEqual(result["reliability"]["acknowledged"], 20)
        self.assertEqual(result["reliability"]["deadline_misses"], 5)
        self.assertTrue(result["preregistration"]["predates_first_run"])

    def test_post_first_deadline_miss_is_rejected(self) -> None:
        rows = [
            {"sequence": 1, "full_loop_us": 99_000},
            {"sequence": 2, "full_loop_us": 100_001},
        ]

        with self.assertRaisesRegex(aggregate.AggregationError, "only sequence 1"):
            aggregate.validate_deadline_contract(rows)

    def test_ort_timing_budget_is_enforced(self) -> None:
        with self.assertRaisesRegex(aggregate.AggregationError, "p99"):
            aggregate.validate_ort_timing_contract(
                [contract.MAX_ORT_WALL_P99_NS + 1] * 100,
                200_000,
            )

    def test_changed_preregistered_threshold_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign_root = write_campaign(Path(temporary))
            path = campaign_root / "preregistration.json"
            document = json.loads(path.read_text(encoding="utf-8"))
            document["frozen_thresholds"]["max_full_loop_us"] += 1
            write_json(path, document)
            (campaign_root / "preregistration.sha256").write_text(
                f"{sha256_file(path)}  preregistration.json\n",
                encoding="utf-8",
                newline="\n",
            )

            with self.assertRaisesRegex(contract.ContractError, "frozen_thresholds"):
                aggregate.aggregate_campaign(
                    campaign_root,
                    SOURCE_COMMIT,
                    expected_runs=5,
                    expected_count=4,
                )


class OrtCampaignRunnerTests(unittest.TestCase):
    def test_dry_run_freezes_five_full_runs(self) -> None:
        commit = subprocess.run(
            ["git", "-C", str(REPOSITORY_ROOT), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        with tempfile.TemporaryDirectory() as temporary:
            result_root = Path(temporary) / "new-ort-campaign"
            environment = os.environ.copy()
            environment["IVC_ORT_PROFILE_RUNNER"] = "/bin/true"
            completed = subprocess.run(
                [
                    "bash",
                    str(IVC_DIR / "run-ort-control-campaign.sh"),
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

        self.assertIn("runs=5 samples_per_run=1800", completed.stdout)
        self.assertIn("ort-full --repeat 5", completed.stdout)
        self.assertFalse(result_root.exists())

    def test_runner_refuses_to_reuse_result_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            environment = os.environ.copy()
            environment["IVC_ORT_PROFILE_RUNNER"] = "/bin/true"
            completed = subprocess.run(
                [
                    "bash",
                    str(IVC_DIR / "run-ort-control-campaign.sh"),
                    "--result-dir",
                    temporary,
                    "--expected-commit",
                    SOURCE_COMMIT,
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
