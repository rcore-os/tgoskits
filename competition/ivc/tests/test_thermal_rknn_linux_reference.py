from __future__ import annotations

import importlib.util
import struct
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np


IVC_DIR = Path(__file__).resolve().parents[1]
MODEL_DIR = IVC_DIR / "model"
SPEC = importlib.util.spec_from_file_location(
    "thermal_rknn_linux_reference",
    MODEL_DIR / "thermal_rknn_linux_reference.py",
)
assert SPEC is not None and SPEC.loader is not None
reference = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = reference
SPEC.loader.exec_module(reference)


REMOTE_DIR = "/home/orangepi/ivc-rknn-reference-unit-test"


def f32_bits(value: float) -> str:
    return f"{struct.unpack('<I', struct.pack('<f', value))[0]:08x}"


def actuator_command(value: float) -> int:
    return int(np.float32(np.float32(value) * np.float32(1000.0)) + np.float32(0.5))


class ThermalRknnLinuxReferenceTests(unittest.TestCase):
    def write_passing_raw(
        self,
        path: Path,
        vectors: list[dict[str, object]],
        outputs: np.ndarray,
    ) -> None:
        rows = [",".join(reference.RAW_HEADER)]
        for index, (vector, output) in enumerate(zip(vectors, outputs, strict=True)):
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

    def write_console(self, path: Path) -> None:
        api = "2.3.2 (429f97ae6b@2025-04-09T09:09:27)".encode().hex()
        driver = "0.9.6".encode().hex()
        input_name = "normalized_observation".encode().hex()
        output_name = "control_fraction".encode().hex()
        path.write_text(
            "\n".join(
                [
                    "IVC_RKNN_LINUX_BEGIN schema=1 vectors=10000 warmup=32 core_mask=0",
                    f"IVC_RKNN_RUNTIME api_version_hex={api} driver_version_hex={driver}",
                    "IVC_RKNN_TENSOR "
                    f"input_name_hex={input_name} input_type=FP16 input_fmt=UNDEFINED "
                    "input_elems=4 submitted_input_type=FP32 "
                    f"output_name_hex={output_name} output_type=FP16 output_fmt=UNDEFINED "
                    "output_elems=1 requested_output_type=FP32",
                    "IVC_RKNN_LINUX_RESULT status=pass vectors=10000 warmup=32 "
                    "core_mask=0 init_us=2000 exact_actuator_matches=10000 "
                    "maximum_absolute_error=0 maximum_absolute_actuator_delta=0 "
                    "perf_query_errors=0 run_errors=0",
                    "IVC_RKNN_LINUX_DONE",
                ]
            )
            + "\n",
            encoding="utf-8",
            newline="\n",
        )

    def write_board_evidence(self, root: Path, corpus: Path) -> reference.EvidenceInputs:
        runner = root / "thermal_rknn_linux_reference"
        runner.write_bytes(b"unit-test-runner")
        ldd = root / "ldd.log"
        ldd.write_text(
            f"librknnrt.so => {REMOTE_DIR}/lib/librknnrt.so (0x00000000)\n",
            encoding="utf-8",
            newline="\n",
        )
        facts = root / "board-facts.txt"
        facts.write_text(
            "\n".join(
                [
                    "schema=1",
                    "hostname=orangepi5plus",
                    "machine=aarch64",
                    "kernel_release=6.1.43-rockchip-rk3588",
                    "rknpu_version=0.9.6",
                    "root_source=/dev/mmcblk1p2",
                    "root_fstype=ext4",
                    "root_options=rw,noatime",
                    f"machine_id_sha256={'b' * 64}",
                    "cpu_temp_start_milli_c=42000",
                    "cpu_temp_finish_milli_c=43000",
                    f"gxx_version_hex={'g++ unit-test 11.4.0'.encode().hex()}",
                    f"runtime_sha256={reference.sha256_file(reference.RUNTIME_PATH)}",
                    f"rknn_sha256={reference.sha256_file(reference.RKNN_PATH)}",
                    f"corpus_sha256={reference.sha256_file(corpus)}",
                    f"runner_sha256={reference.sha256_file(runner)}",
                ]
            )
            + "\n",
            encoding="utf-8",
            newline="\n",
        )
        return reference.EvidenceInputs(
            board_facts_path=facts,
            ldd_path=ldd,
            deployed_runtime_path=reference.RUNTIME_PATH,
            deployed_model_path=reference.RKNN_PATH,
            runner_binary_path=runner,
            board_type="OrangePi-5-Plus",
            remote_dir=REMOTE_DIR,
            run_id="unit-test",
            source_commit="a" * 40,
            source_branch="experiment/unit-test",
            source_dirty=False,
            tracked_change_count=0,
            untracked_file_count=0,
            started_at="2026-08-04T00:00:00Z",
            finished_at="2026-08-04T00:01:00Z",
            require_clean_source=True,
        )

    def test_prepare_and_analyze_complete_physical_evidence(self) -> None:
        documents = reference.load_documents()
        inputs = reference.decode_inputs(documents["vectors"])
        outputs = reference.fp16_oracle(documents["weights"], inputs)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus = root / "corpus.csv"
            raw = root / "raw.csv"
            console = root / "console.log"
            report_path = root / "report.json"
            prepared = reference.prepare_corpus(corpus, check=False)
            checked = reference.prepare_corpus(corpus, check=True)
            self.assertEqual(prepared["sha256"], checked["sha256"])
            self.assertEqual(prepared["vectors"], 10_000)
            self.write_passing_raw(raw, documents["vectors"], outputs)
            self.write_console(console)
            evidence = self.write_board_evidence(root, corpus)

            report = reference.analyze(raw, console, corpus, report_path, evidence)

            self.assertEqual(report["status"], "pass")
            self.assertEqual(report["vectors"], 10_000)
            self.assertTrue(report["physical_evidence"]["deployment"]["uses_frozen_runtime"])
            self.assertFalse(report["physical_evidence"]["source"]["dirty"])
            self.assertEqual(
                report["physical_evidence"]["deployment"]["runtime"]["path"],
                "apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/"
                "3rdparty/rknpu2/Linux/aarch64/librknnrt.so",
            )
            self.assertEqual(
                report["physical_evidence"]["deployment"]["corpus"]["path"],
                "board/corpus.csv",
            )
            self.assertEqual(
                report["physical_evidence"]["deployment"]["resolved_runtime_path"],
                f"{REMOTE_DIR}/lib/librknnrt.so",
            )

    def test_non_positive_device_time_is_rejected(self) -> None:
        vector = reference.load_documents()["vectors"][0]
        with tempfile.TemporaryDirectory() as directory:
            raw = Path(directory) / "raw.csv"
            row = [
                "0",
                *vector["normalized_input_f32_bits"],
                vector["output_f32_bits"],
                str(vector["actuator_permille"]),
                vector["output_f32_bits"],
                str(vector["actuator_permille"]),
                "100",
                "0",
            ]
            raw.write_text(
                ",".join(reference.RAW_HEADER) + "\n" + ",".join(row) + "\n",
                encoding="utf-8",
                newline="\n",
            )
            with self.assertRaisesRegex(reference.ReferenceError, "device time is not positive"):
                reference.read_raw(raw, [vector])


if __name__ == "__main__":
    unittest.main()
