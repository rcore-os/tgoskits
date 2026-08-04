from __future__ import annotations

import importlib.util
import shutil
import struct
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np


IVC_DIR = Path(__file__).resolve().parents[1]
MODEL_DIR = IVC_DIR / "model"
SPEC = importlib.util.spec_from_file_location(
    "thermal_rknn_starry_reference",
    MODEL_DIR / "thermal_rknn_starry_reference.py",
)
assert SPEC is not None and SPEC.loader is not None
starry_reference = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = starry_reference
SPEC.loader.exec_module(starry_reference)
reference = starry_reference.reference


def f32_bits(value: float) -> str:
    return f"{struct.unpack('<I', struct.pack('<f', value))[0]:08x}"


def actuator_command(value: float) -> int:
    scaled = np.float32(np.float32(value) * np.float32(1000.0))
    return int(scaled + np.float32(0.5))


class ThermalRknnStarryReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.documents = reference.load_documents()
        inputs = reference.decode_inputs(cls.documents["vectors"])
        cls.outputs = reference.fp16_oracle(cls.documents["weights"], inputs)

    def write_raw(self, path: Path) -> None:
        rows = [",".join(reference.RAW_HEADER)]
        for index, (vector, output) in enumerate(
            zip(self.documents["vectors"], self.outputs, strict=True)
        ):
            output_value = float(np.float32(output))
            rows.append(
                ",".join(
                    [
                        str(index),
                        *vector["normalized_input_f32_bits"],
                        vector["output_f32_bits"],
                        str(vector["actuator_permille"]),
                        f32_bits(output_value),
                        str(actuator_command(output_value)),
                        str(200_000 + index),
                        str(150 + index % 7),
                    ]
                )
            )
        path.write_text("\n".join(rows) + "\n", encoding="utf-8", newline="\n")

    def write_console(
        self,
        path: Path,
        raw_sha256: str,
        model_sha256: str,
        corpus_sha256: str,
        runtime_sha256: str,
    ) -> None:
        api_version = "2.3.2 (unit-test)".encode().hex()
        driver_version = "0.9.8".encode().hex()
        native_outputs = np.asarray(
            [vector["output"] for vector in self.documents["vectors"]],
            dtype=np.float32,
        )
        native_commands = np.asarray(
            [vector["actuator_permille"] for vector in self.documents["vectors"]],
            dtype=np.int32,
        )
        rknn_outputs = self.outputs.astype(np.float32)
        rknn_commands = np.asarray(
            [actuator_command(float(output)) for output in rknn_outputs],
            dtype=np.int32,
        )
        maximum_error = float(
            np.max(
                np.abs(
                    rknn_outputs.astype(np.float64)
                    - native_outputs.astype(np.float64)
                )
            )
        )
        deltas = rknn_commands - native_commands
        exact_matches = int(np.count_nonzero(deltas == 0))
        maximum_delta = int(np.max(np.abs(deltas)))
        lines = [
            "AXVISOR_RK3588_NPU_HANDOFF_READY cores=3 power_domains=3 clocks=8 "
            "resets=6 scmi_clock_id=6 scmi_rate_hz=200000000 host_submit=false",
            "NPU registered successfully",
            "THERMAL_RKNN_STARRY_BEGIN schema=1 vectors=10000 warmup=32 "
            "core_mask=0 backend=rknn-npu",
            "IVC_RKNN_RUNTIME api_version_hex=00",
            f"IVC_RKNN_RUNTIME api_version_hex={api_version} "
            f"driver_version_hex={driver_version}",
        ]
        lines.extend(
            f"IVC_RKNN_PROGRESS completed={completed}"
            for completed in range(1000, 10_001, 1000)
        )
        lines.extend(
            [
                "IVC_RKNN_LINUX_RESULT status=pass vectors=10000 warmup=32 "
                f"core_mask=0 init_us=78179 exact_actuator_matches={exact_matches} "
                f"maximum_absolute_error={maximum_error} "
                f"maximum_absolute_actuator_delta={maximum_delta} "
                "perf_query_errors=0 run_errors=0",
                "THERMAL_RKNN_STARRY_PAS? schema=1 vectors=10000",
                "THERMAL_RKNN_STARRY_PASS schema=1 vectors=10000 warmup=32 "
                "core_mask=0 backend=rknn-npu "
                f"model_sha256={model_sha256} corpus_sha256={corpus_sha256} "
                f"runtime_sha256={runtime_sha256} raw_sha256={raw_sha256}",
                "THERMAL_RKNN_STARRY_PASS schema=1 vectors=9999 "
                "THERMAL_RKNN_STARRY_PASS schema=1 vectors=10000 warmup=32 "
                "core_mask=0 backend=rknn-npu "
                f"model_sha256={model_sha256} corpus_sha256={corpus_sha256} "
                f"runtime_sha256={runtime_sha256} raw_sha256={raw_sha256}",
                "AXVISOR_SNAPSHOT_SYNC_OK",
                "AXVISOR_HOST_FILESYSTEM_SYNCED",
                "AXVISOR_HOST_FILESYSTEM_SYNCED",
                "=== SUCCESS PATTERN MATCHED: "
                "(?m)^AXVISOR_HOST_FILESYSTEM_SYNCED\\r?$ ===",
            ]
        )
        path.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")

    def prepare_evidence(
        self,
        root: Path,
    ) -> tuple[starry_reference.StarryEvidenceInputs, Path]:
        raw = root / "raw.csv"
        raw_manifest = root / "raw.csv.sha256"
        console = root / "console.log"
        profile = root / "rknpu-offline-profile"
        board_facts = root / "board-facts.txt"
        snapshot = root / "starry-rknpu-result.img"
        embedded_runner = root / "embedded-runner"
        embedded_corpus = root / "embedded-corpus.csv"
        built_runner = root / "built-runner"
        report = root / "starry-reference.json"

        built_runner.write_bytes(b"unit-test-aarch64-runner")
        shutil.copyfile(built_runner, embedded_runner)
        embedded_corpus.write_bytes(reference.corpus_bytes(self.documents["vectors"]))
        self.write_raw(raw)
        raw_sha256 = reference.sha256_file(raw)
        raw_manifest.write_text(
            f"{raw_sha256}  /var/lib/rknn/raw.csv\n",
            encoding="utf-8",
            newline="\n",
        )
        model_sha256 = reference.sha256_file(reference.RKNN_PATH)
        corpus_sha256 = reference.sha256_file(embedded_corpus)
        runtime_sha256 = reference.sha256_file(reference.RUNTIME_PATH)
        profile.write_text(
            "\n".join(
                [
                    "schema=1",
                    "vectors=10000",
                    "warmup=32",
                    "core_mask=0",
                    f"runner_sha256={reference.sha256_file(built_runner)}",
                    f"model_sha256={model_sha256}",
                    f"corpus_sha256={corpus_sha256}",
                    f"runtime_sha256={runtime_sha256}",
                ]
            )
            + "\n",
            encoding="utf-8",
            newline="\n",
        )
        with snapshot.open("wb") as image:
            image.truncate(starry_reference.EXPECTED_SNAPSHOT_BYTES)
        snapshot_sha256 = reference.sha256_file(snapshot)
        board_facts.write_text(
            "\n".join(
                [
                    "hostname=orangepi5plus",
                    "machine=aarch64",
                    "kernel_release=6.1.43-rockchip-rk3588",
                    "root_source=/dev/mmcblk1p2",
                    "root_fstype=ext4",
                    f"snapshot_sha256={snapshot_sha256}",
                    f"snapshot_size={starry_reference.EXPECTED_SNAPSHOT_BYTES}",
                ]
            )
            + "\n",
            encoding="utf-8",
            newline="\n",
        )
        self.write_console(
            console,
            raw_sha256,
            model_sha256,
            corpus_sha256,
            runtime_sha256,
        )
        evidence = starry_reference.StarryEvidenceInputs(
            raw_path=raw,
            raw_manifest_path=raw_manifest,
            console_path=console,
            profile_path=profile,
            board_facts_path=board_facts,
            snapshot_path=snapshot,
            embedded_runner_path=embedded_runner,
            embedded_model_path=reference.RKNN_PATH,
            embedded_corpus_path=embedded_corpus,
            embedded_runtime_path=reference.RUNTIME_PATH,
            built_runner_path=built_runner,
            run_id="unit-test",
            source_commit="a" * 40,
            source_branch="experiment/unit-test",
            source_provenance="captured-before-run",
            source_dirty=False,
            tracked_change_count=0,
            untracked_file_count=0,
            started_at="2026-08-04T00:00:00Z",
            finished_at="2026-08-04T00:01:00Z",
            require_clean_source=True,
        )
        return evidence, report

    def test_complete_physical_starry_evidence_and_bounded_termination(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence, report_path = self.prepare_evidence(Path(directory))

            report = starry_reference.analyze(evidence, report_path)

            self.assertEqual(report["status"], "pass")
            self.assertEqual(report["vectors"], 10_000)
            self.assertTrue(report["backend"]["guest_exclusive_handoff"])
            self.assertEqual(report["source"]["provenance"], "captured-before-run")
            self.assertEqual(report["backend"]["positive_device_time_samples"], 10_000)
            self.assertEqual(report["latency"]["device"]["p99"], 156)
            self.assertEqual(report["console_evidence"]["valid_pass_marker_copies"], 2)
            self.assertEqual(report["console_evidence"]["host_sync_marker_copies"], 2)
            self.assertTrue(report_path.is_file())

            console = evidence.console_path.read_text(encoding="utf-8")
            evidence.console_path.write_text(
                console.replace(
                    "=== SUCCESS PATTERN MATCHED: "
                    "(?m)^AXVISOR_HOST_FILESYSTEM_SYNCED\\r?$ ===\n",
                    "",
                ),
                encoding="utf-8",
                newline="\n",
            )
            artifacts = starry_reference.validate_artifacts(evidence, self.documents)
            with self.assertRaisesRegex(
                reference.ReferenceError,
                "bounded final sync marker",
            ):
                starry_reference.validate_console(evidence.console_path, artifacts)

    def test_non_utf8_boot_bytes_do_not_hide_ascii_evidence_markers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence, report_path = self.prepare_evidence(Path(directory))
            console = evidence.console_path.read_bytes()
            evidence.console_path.write_bytes(b"\xff\xfeDDR training\n" + console)

            report = starry_reference.analyze(evidence, report_path)

            self.assertEqual(report["status"], "pass")
            self.assertEqual(
                report["console_evidence"]["utf8_replacement_characters"],
                2,
            )

    def test_compact_pass_and_raw_markers_survive_long_line_loss(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence, report_path = self.prepare_evidence(Path(directory))
            console_lines = [
                line
                for line in evidence.console_path.read_text(
                    encoding="utf-8"
                ).splitlines()
                if "THERMAL_RKNN_STARRY_PASS" not in line
            ]
            compact_pass = (
                "THERMAL_RKNN_STARRY_PASS schema=1 vectors=10000 warmup=32 "
                "core_mask=0 backend=rknn-npu"
            )
            compact_raw = (
                "THERMAL_RKNN_STARRY_RAW schema=1 vectors=10000 sha256="
                f"{reference.sha256_file(evidence.raw_path)}"
            )
            console_lines.extend(
                [compact_pass, compact_raw, compact_pass, compact_raw]
            )
            evidence.console_path.write_text(
                "\n".join(console_lines) + "\n",
                encoding="utf-8",
                newline="\n",
            )

            report = starry_reference.analyze(evidence, report_path)

            self.assertEqual(report["status"], "pass")
            self.assertEqual(
                report["console_evidence"]["valid_pass_marker_copies"],
                2,
            )
            self.assertEqual(
                report["console_evidence"]["valid_raw_marker_copies"],
                2,
            )

    def test_compact_runtime_and_result_markers_survive_long_line_loss(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence, report_path = self.prepare_evidence(Path(directory))
            console = evidence.console_path.read_text(encoding="utf-8")
            runtime = next(
                fields
                for fields in starry_reference.marker_candidates(
                    console, "IVC_RKNN_RUNTIME"
                )
                if "driver_version_hex" in fields
            )
            result = starry_reference.marker_candidates(
                console, "IVC_RKNN_LINUX_RESULT"
            )[0]
            console_lines = [
                line
                for line in console.splitlines()
                if "IVC_RKNN_RUNTIME" not in line
                and "IVC_RKNN_LINUX_RESULT" not in line
            ]
            compact_markers = [
                "IVC_RKNN_RUNTIME_API version_hex="
                f"{runtime['api_version_hex']}",
                "IVC_RKNN_RUNTIME_DRIVER version_hex="
                f"{runtime['driver_version_hex']}",
                "IVC_RKNN_RESULT_META status=pass vectors=10000 warmup=32 "
                f"core_mask=0 init_us={result['init_us']}",
                "IVC_RKNN_RESULT_ACCURACY exact_actuator_matches="
                f"{result['exact_actuator_matches']} "
                "maximum_absolute_actuator_delta="
                f"{result['maximum_absolute_actuator_delta']}",
                "IVC_RKNN_RESULT_ERROR maximum_absolute_error="
                f"{result['maximum_absolute_error']}",
                "IVC_RKNN_RESULT_HEALTH perf_query_errors=0 run_errors=0",
            ]
            console_lines.extend(compact_markers * 2)
            evidence.console_path.write_text(
                "\n".join(console_lines) + "\n",
                encoding="utf-8",
                newline="\n",
            )

            report = starry_reference.analyze(evidence, report_path)

            self.assertEqual(report["status"], "pass")
            self.assertEqual(
                report["console_evidence"]["compact_runtime_marker_sets"],
                2,
            )
            self.assertEqual(
                report["console_evidence"]["compact_result_marker_sets"],
                2,
            )

    def test_compact_runtime_ignores_more_frequent_malformed_copies(self) -> None:
        api_version = "2.3.2 (unit-test)"
        api_version_hex = api_version.encode().hex()
        driver_version = "0.9.8"
        driver_version_hex = driver_version.encode().hex()
        damaged_api_version_hex = api_version_hex[:-8]
        uart_prefix = "[guest-console:pl011-starry]"
        damaged_api_marker = (
            "IVC_RKNN_RUNTIME_API version_hex="
            f"{damaged_api_version_hex}{uart_prefix}"
        )
        valid_api_marker = (
            f"IVC_RKNN_RUNTIME_API version_hex={api_version_hex}"
        )
        driver_marker = (
            f"IVC_RKNN_RUNTIME_DRIVER version_hex={driver_version_hex}"
        )
        console = "\n".join(
            [
                f"{uart_prefix} {damaged_api_marker} {damaged_api_marker} "
                f"{valid_api_marker}",
                f"{uart_prefix} {damaged_api_marker} {valid_api_marker}",
                *([driver_marker] * 5),
            ]
        )

        selected_api, selected_driver, legacy_copies, compact_sets = (
            starry_reference.matching_runtime_marker(console)
        )

        self.assertEqual(selected_api, api_version)
        self.assertEqual(selected_driver, driver_version)
        self.assertEqual(legacy_copies, 2)
        self.assertEqual(compact_sets, 2)


if __name__ == "__main__":
    unittest.main()
