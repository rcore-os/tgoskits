from __future__ import annotations

import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path


BENCHMARK_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCHMARK_DIR))
SPEC = importlib.util.spec_from_file_location(
    "axvisor_rt_formal_campaign", BENCHMARK_DIR / "formal_campaign.py"
)
assert SPEC is not None and SPEC.loader is not None
campaign = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(campaign)


class FormalCampaignContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.workspace = Path(self.temporary.name) / "workspace"
        self.workspace.mkdir()
        self.git("init")
        self.git("config", "user.email", "rt-campaign@example.invalid")
        self.git("config", "user.name", "RT Campaign Test")
        (self.workspace / ".gitignore").write_text("/tmp/*\n", encoding="utf-8")
        for relative in campaign.FROZEN_SOURCE_PATHS:
            path = self.workspace / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"fixture for {relative}\n", encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-m", "fixture source")
        self.commit = self.git("rev-parse", "HEAD").strip()
        artifact_root = self.workspace / "tmp" / "formal-artifacts"
        artifact_root.mkdir(parents=True)
        self.artifacts: dict[str, Path] = {}
        for name in campaign.ARTIFACT_NAMES:
            if name == "host_toolchain":
                continue
            path = artifact_root / f"{name}.bin"
            path.write_bytes((name + "\n").encode() * 3)
            self.artifacts[name] = path
        self.host_compiler = artifact_root / "aarch64-linux-gnu-gcc"
        self.host_archiver = artifact_root / "aarch64-linux-gnu-ar"
        self.host_sysroot = artifact_root / "aarch64-sysroot"
        self.host_sysroot.mkdir()
        self.host_compiler.write_text(
            "#!/bin/sh\n"
            "case \"$*\" in\n"
            "  -dumpmachine) echo aarch64-linux-gnu ;;\n"
            "  '-dumpfullversion -dumpversion') echo 11.4.0-test ;;\n"
            f"  -print-sysroot) echo {self.host_sysroot} ;;\n"
            "  *) exit 2 ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        self.host_archiver.write_text(
            "#!/bin/sh\n"
            "[ \"${1:-}\" = --version ] || exit 2\n"
            "echo 'GNU ar test'\n",
            encoding="utf-8",
        )
        self.host_compiler.chmod(0o755)
        self.host_archiver.chmod(0o755)

        def file_record(path: Path, *, version: str | None = None) -> dict[str, object]:
            record: dict[str, object] = {
                "path": str(path.resolve()),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "size_bytes": path.stat().st_size,
            }
            if version is not None:
                record["version"] = version
            return record

        host_toolchain = artifact_root / "host-toolchain.json"
        host_toolchain.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "purpose": "StarryOS freestanding C objects and bindings",
                    "target": {
                        "machine": "aarch64-linux-gnu",
                        "sysroot": str(self.host_sysroot.resolve()),
                    },
                    "compiler": file_record(
                        self.host_compiler, version="11.4.0-test"
                    ),
                    "archiver": file_record(
                        self.host_archiver, version="GNU ar test"
                    ),
                    "wrappers": {
                        "compiler": file_record(self.host_compiler),
                        "archiver": file_record(self.host_archiver),
                    },
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        self.artifacts["host_toolchain"] = host_toolchain
        self.document = campaign.build_preregistration(
            workspace=self.workspace,
            expected_commit=self.commit,
            source_ref="feat/rt-device-graph",
            board_type="OrangePi-5-Plus",
            service_id="orangepi-5-plus-1",
            hardware_id="bf61f4d4a1d994ad",
            hostname="orangepi5plus",
            artifacts=self.artifacts,
            created_at=datetime(2026, 8, 12, 4, 5, 6, tzinfo=timezone.utc),
            pair_timeout_seconds=900,
            soak_timeout_seconds=4500,
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def git(self, *arguments: str) -> str:
        return subprocess.run(
            ["git", "-C", str(self.workspace), *arguments],
            check=True,
            capture_output=True,
            text=True,
        ).stdout

    def test_preregistration_binds_source_artifacts_and_order(self) -> None:
        self.assertEqual(self.document["schema_version"], 2)
        self.assertIn("host_toolchain", campaign.ARTIFACT_NAMES)
        self.assertIn(
            "scripts/benchmark/axvisor-rt/prepare-freestanding-c-toolchain.sh",
            campaign.FROZEN_SOURCE_PATHS,
        )
        self.assertEqual(self.document["source"]["commit"], self.commit)
        self.assertEqual(
            [entry["order"] for entry in self.document["pair_order"]],
            [
                ["shared", "partitioned"],
                ["partitioned", "shared"],
                ["shared", "partitioned"],
                ["partitioned", "shared"],
                ["shared", "partitioned"],
            ],
        )
        expected = hashlib.sha256(self.artifacts["pair_kernel"].read_bytes()).hexdigest()
        self.assertEqual(self.document["artifacts"]["pair_kernel"]["sha256"], expected)
        campaign.validate_preregistration(self.document, self.workspace, require_clean=True)

    def test_validation_rejects_dirty_source_and_mutated_artifact(self) -> None:
        source = self.workspace / campaign.FROZEN_SOURCE_PATHS[0]
        source.write_text("mutated source\n", encoding="utf-8")
        with self.assertRaisesRegex(campaign.ContractError, "clean Git worktree"):
            campaign.validate_preregistration(
                self.document, self.workspace, require_clean=True
            )

    def test_validation_rejects_mutated_host_compiler(self) -> None:
        original = self.host_compiler.read_bytes()
        self.host_compiler.write_bytes(bytes([original[0] ^ 1]) + original[1:])
        with self.assertRaisesRegex(campaign.ContractError, "compiler.*SHA-256"):
            campaign.validate_preregistration(
                self.document, self.workspace, require_clean=True
            )

    def test_validation_rejects_modified_acceptance_and_invalid_timeout(self) -> None:
        modified_acceptance = json.loads(json.dumps(self.document))
        modified_acceptance["acceptance"][
            "direct_irq_p99_non_regression_limit_percent"
        ] = 50.0
        with self.assertRaisesRegex(campaign.ContractError, "acceptance"):
            campaign.validate_preregistration(
                modified_acceptance, self.workspace, require_clean=True
            )

        invalid_timeout = json.loads(json.dumps(self.document))
        invalid_timeout["timeouts_seconds"]["soak"] = 0
        with self.assertRaisesRegex(campaign.ContractError, "timeout"):
            campaign.validate_preregistration(
                invalid_timeout, self.workspace, require_clean=True
            )
        self.git("checkout", "--", campaign.FROZEN_SOURCE_PATHS[0])
        original = self.artifacts["pair_rootfs"].read_bytes()
        self.artifacts["pair_rootfs"].write_bytes(b"X" + original[1:])
        with self.assertRaisesRegex(campaign.ContractError, "pair_rootfs.*SHA-256"):
            campaign.validate_preregistration(
                self.document, self.workspace, require_clean=True
            )

    def test_stage_identity_must_match_preregistered_board(self) -> None:
        valid = "\n".join(
            (
                "AXVISOR_RT_BOARD_SERVICE_ID board_type=OrangePi-5-Plus "
                "board_id=orangepi-5-plus-1",
                "/dev/mmcblk1p2 ext4 rw,noatime,errors=remount-ro",
                "AXVISOR_RT_BOARD_IDENTITY board_id=bf61f4d4a1d994ad "
                "hostname=orangepi5plus cpu_temp_milli_c=42538",
                "AXVISOR_RT_BOARD_STAGE_PASS",
                "AXVISOR_RT_BOARD_STAGE_COMPLETE destination=/home/orangepi/axvisor-guest "
                "rootfs=starry-rt-capture-rootfs.img noise=none",
            )
        )
        identity = campaign.validate_stage_log(self.document, valid)
        self.assertEqual(identity["board_id"], "bf61f4d4a1d994ad")
        changed = valid.replace("bf61f4d4a1d994ad", "0000000000000000")
        with self.assertRaisesRegex(campaign.ContractError, "hardware ID"):
            campaign.validate_stage_log(self.document, changed)

    def test_next_slot_rejects_out_of_order_receipt(self) -> None:
        result_root = Path(self.temporary.name) / "results"
        result_root.mkdir()
        first = campaign.next_slot(self.document, result_root)
        assert first is not None
        self.assertEqual((first.phase, first.pair, first.profile), ("pair", 1, "shared"))
        later = campaign.campaign_slots()[1]
        receipt = campaign.receipt_path(result_root, later)
        receipt.parent.mkdir(parents=True)
        receipt.write_text("{}\n", encoding="utf-8")
        with self.assertRaisesRegex(campaign.ContractError, "out-of-order"):
            campaign.next_slot(self.document, result_root)

    def test_next_slot_rejects_invalid_completed_receipt(self) -> None:
        result_root = Path(self.temporary.name) / "results"
        first = campaign.campaign_slots()[0]
        receipt = campaign.receipt_path(result_root, first)
        receipt.parent.mkdir(parents=True)
        receipt.write_text("{}\n", encoding="utf-8")
        with self.assertRaisesRegex(campaign.ContractError, "receipt"):
            campaign.next_slot(self.document, result_root)

    def test_next_slot_rejects_mutated_archived_evidence(self) -> None:
        arguments = self.receipt_arguments()
        receipt = campaign.build_receipt(**arguments)
        receipt_path = campaign.receipt_path(
            arguments["result_root"], arguments["slot"]
        )
        receipt_path.parent.mkdir(parents=True, exist_ok=True)
        receipt_path.write_text(json.dumps(receipt), encoding="utf-8")
        arguments["raw_path"].write_bytes(b"bad evidence\n")
        with self.assertRaisesRegex(campaign.ContractError, "raw.*SHA-256"):
            campaign.next_slot(self.document, arguments["result_root"])

    def test_pair_and_soak_summaries_are_distinct(self) -> None:
        campaign.validate_summary(
            self.document, make_summary("shared", False), "shared", soak=False
        )
        soak = make_summary("partitioned", True)
        campaign.validate_summary(self.document, soak, "partitioned", soak=True)
        soak["host_noise"]["elapsed_ns"] = 1_799_999_999_999
        with self.assertRaisesRegex(campaign.ContractError, "1,800 seconds"):
            campaign.validate_summary(self.document, soak, "partitioned", soak=True)

    def test_receipt_rejects_changed_harvest_board(self) -> None:
        arguments = self.receipt_arguments()
        arguments["harvest_log"].write_text(
            "AXVISOR_RT_BOARD_IDENTITY board_id=0000000000000000 "
            "hostname=orangepi5plus cpu_temp_milli_c=43000\n"
            "AXVISOR_RT_STARRY_HARVESTED profile=shared\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(campaign.ContractError, "harvest hardware ID"):
            campaign.build_receipt(**arguments)

    def test_receipt_rejects_summary_raw_hash_mismatch(self) -> None:
        arguments = self.receipt_arguments()
        summary = json.loads(arguments["summary_path"].read_text(encoding="utf-8"))
        summary["input"]["sha256"] = "f" * 64
        arguments["summary_path"].write_text(
            json.dumps(summary), encoding="utf-8"
        )
        with self.assertRaisesRegex(campaign.ContractError, "raw SHA-256"):
            campaign.build_receipt(**arguments)

    def test_receipt_accepts_non_utf8_serial_noise(self) -> None:
        arguments = self.receipt_arguments()
        console = arguments["console_log"]
        assert isinstance(console, Path)
        console.write_bytes(
            b"firmware-prefix:\x00\xfe\n"
            + console.read_bytes()
            + b"firmware-suffix:\xff\x00\n"
        )

        receipt = campaign.build_receipt(**arguments)

        self.assertEqual(receipt["slot"]["profile"], "shared")

    def receipt_arguments(self) -> dict[str, object]:
        result_root = Path(self.temporary.name) / "results"
        attempt = result_root / "pair-1" / "shared" / "attempts" / "fixture"
        attempt.mkdir(parents=True)
        stage = attempt / "stage.log"
        stage.write_text(
            "AXVISOR_RT_BOARD_SERVICE_ID board_type=OrangePi-5-Plus "
            "board_id=orangepi-5-plus-1\n"
            "/dev/mmcblk1p2 ext4 rw,noatime\n"
            "AXVISOR_RT_BOARD_IDENTITY board_id=bf61f4d4a1d994ad "
            "hostname=orangepi5plus cpu_temp_milli_c=42000\n"
            "AXVISOR_RT_BOARD_STAGE_PASS\n"
            "AXVISOR_RT_BOARD_STAGE_COMPLETE destination=/home/orangepi/axvisor-guest\n",
            encoding="utf-8",
        )
        console = attempt / "console.log"
        console.write_text(
            "  board_id: orangepi-5-plus-1\n"
            "axvisor-orangepi-5-plus-starry-host-noise-formal-shared.toml\n"
            "AXVISOR_RT_STARRY_CAPTURE_COMPLETE schema=1 workload=idle\n"
            "AXVISOR_SNAPSHOT_SYNC_OK\n"
            "/dev/mmcblk1p2 ext4 rw,noatime\n"
            "BOARD_LINUX_RESTORED\n",
            encoding="utf-8",
        )
        harvest = attempt / "harvest.log"
        harvest.write_text(
            "AXVISOR_RT_BOARD_IDENTITY board_id=bf61f4d4a1d994ad "
            "hostname=orangepi5plus cpu_temp_milli_c=43000\n"
            "AXVISOR_RT_STARRY_HARVESTED profile=shared\n",
            encoding="utf-8",
        )
        raw = attempt / "raw.log"
        guest_irq = attempt / "guest-irq.log.gz"
        host_trace = attempt / "host.log"
        raw.write_bytes(b"raw evidence\n")
        guest_irq.write_bytes(b"guest irq evidence\n")
        host_trace.write_bytes(b"host trace evidence\n")
        summary = make_summary("shared", False)
        summary["input"]["path"] = str(raw)
        summary["input"]["sha256"] = campaign.sha256_file(raw)
        summary["direct_irq_trace"]["inputs"] = {
            "guest": {"path": str(guest_irq), "sha256": campaign.sha256_file(guest_irq)},
            "host": {"path": str(host_trace), "sha256": campaign.sha256_file(host_trace)},
        }
        summary_path = attempt / "summary.json"
        summary_path.write_text(json.dumps(summary), encoding="utf-8")
        return {
            "preregistration": self.document,
            "result_root": result_root,
            "slot": campaign.Slot("pair", 1, "shared"),
            "stage_log": stage,
            "console_log": console,
            "harvest_log": harvest,
            "summary_path": summary_path,
            "raw_path": raw,
            "guest_irq_path": guest_irq,
            "host_trace_path": host_trace,
            "started_at": "2026-08-12T04:06:00Z",
            "finished_at": "2026-08-12T04:07:00Z",
        }


def make_summary(profile: str, soak: bool) -> dict[str, object]:
    pcpu = 1 if profile == "shared" else 3
    period_us = 90_000 if soak else 1_000
    duration_ms = 3_600_000 if soak else 600_000
    vm_prefix = "starry-orangepi-5-plus-smp2-soak-" if soak else "starry-orangepi-5-plus-smp2-"
    counters = {
        "records": 123,
        "dropped": 0,
        "incomplete": 0,
        "failed_injections": 0,
        "unowned_virtual_timer_irqs": 0,
        "counter_frequency_mismatches": 0,
    }
    return {
        "schema_version": 1,
        "capture": {
            "os": "starryos",
            "platform": "OrangePi-5-Plus",
            "profile": profile,
            "workload": "idle",
            "vcpu_count": 2,
            "iterations_per_metric": 10_000,
            "sample_count": 30_000,
            "warmup_iterations": 100,
            "period_us": period_us,
            "measurement_cpu": 0,
            "stress_cpu": 1,
            "fifo_priority": 80,
        },
        "input": {
            "path": "/evidence/raw.log",
            "sha256": "1" * 64,
            "line_count": 30_012,
            "snapshot_filesystem_state": "clean",
        },
        "profile_contract": {
            "dedicated_cpus": profile == "partitioned",
            "phys_cpu_sets": ["0x2", "0x4"],
            "soak": soak,
            "vm_config": (
                "scripts/benchmark/axvisor-rt/config/" f"{vm_prefix}{profile}.toml"
            ),
        },
        "host_noise": {
            "status": "collected",
            "requested_pcpu": pcpu,
            "observed_pcpu_mask": 1 << pcpu,
            "affinity_mask": 1 << pcpu,
            "pcpus": [{"pcpu": pcpu, "observed_wall_ticks": 42}],
            "covers_host_trace": True,
            "elapsed_ns": 1_900_000_000_000 if soak else 10_000_000,
            "max_duration_ms": duration_ms,
            "stop_reason": "guest-complete",
        },
        "host_pcpu_accounting": {
            "status": "collected",
            "vcpus": [
                {"vm": 1, "vcpu": 0, "pcpu_mask": 2, "migrations": 0},
                {"vm": 1, "vcpu": 1, "pcpu_mask": 4, "migrations": 0},
            ],
        },
        "direct_irq_trace": {
            "pairing": {"pair_count": 123},
            "lossless": {"host": dict(counters), "guest": dict(counters)},
        },
    }


if __name__ == "__main__":
    unittest.main()
