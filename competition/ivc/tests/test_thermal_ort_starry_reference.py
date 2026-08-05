from __future__ import annotations

import importlib.util
import struct
import sys
import tempfile
import unittest
from pathlib import Path


MODEL_DIR = Path(__file__).resolve().parents[1] / "model"
SPEC = importlib.util.spec_from_file_location(
    "ivc_thermal_ort_starry_reference",
    MODEL_DIR / "thermal_ort_starry_reference.py",
)
assert SPEC is not None and SPEC.loader is not None
reference = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = reference
SPEC.loader.exec_module(reference)


def f32_hex(value: float) -> str:
    return struct.pack("!f", value).hex()


class ThermalOrtStarryReferenceTests(unittest.TestCase):
    def write_vector_fixture(self, directory: Path) -> tuple[Path, Path]:
        corpus_path = directory / "corpus.csv"
        raw_path = directory / "raw.csv"
        corpus_lines = [reference.CORPUS_HEADER]
        raw_lines = [reference.RAW_HEADER]
        for index in range(reference.EXPECTED_VECTORS):
            if index == 5743:
                expected_output = f32_hex(0.47449997067451477)
                actual_output = f32_hex(0.47450003027915955)
                expected_command = 474
                actual_command = 475
            else:
                expected_output = f32_hex(0.5)
                actual_output = expected_output
                expected_command = 500
                actual_command = 500
            corpus_fields = [
                str(index),
                "00000000",
                "00000000",
                "00000000",
                "00000000",
                expected_output,
                str(expected_command),
            ]
            corpus_lines.append(",".join(corpus_fields))
            raw_lines.append(
                ",".join(corpus_fields + [actual_output, str(actual_command), "100"])
            )
        corpus_path.write_text("\n".join(corpus_lines) + "\n", encoding="utf-8")
        raw_path.write_text("\n".join(raw_lines) + "\n", encoding="utf-8")
        return raw_path, corpus_path

    def test_raw_gate_accepts_only_the_frozen_rounding_equivalence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            raw_path, corpus_path = self.write_vector_fixture(
                Path(temporary_directory)
            )

            report = reference.analyze_raw(raw_path, corpus_path)

        self.assertEqual(report["vectors"], 10_000)
        self.assertEqual(report["exact_actuator_matches"], 9_999)
        self.assertEqual(report["rounding_boundary_equivalences"], 1)
        self.assertEqual(report["material_actuator_mismatches"], 0)
        self.assertEqual(report["latency"]["p99"], 100)

    def test_raw_gate_rejects_a_material_command_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            raw_path, corpus_path = self.write_vector_fixture(
                Path(temporary_directory)
            )
            raw_lines = raw_path.read_text(encoding="utf-8").splitlines()
            fields = raw_lines[1].split(",")
            fields[8] = "501"
            raw_lines[1] = ",".join(fields)
            raw_path.write_text("\n".join(raw_lines) + "\n", encoding="utf-8")

            with self.assertRaisesRegex(reference.EvidenceError, "material"):
                reference.analyze_raw(raw_path, corpus_path)

    def test_uart_marker_parser_tolerates_prefixes_and_ansi(self) -> None:
        console = (
            "\x1b[32m[guest-console:pl011-starry]\x1b[0m "
            "THERMAL_ORT_STARRY_PASS schema=1 vectors=10000 "
            "backend=onnxruntime-cpu\n"
        )

        copies = reference.matching_marker_copies(
            console,
            "THERMAL_ORT_STARRY_PASS",
            {
                "schema": "1",
                "vectors": "10000",
                "backend": "onnxruntime-cpu",
            },
        )

        self.assertEqual(copies, 1)

    def test_percentile_matches_the_runner_nearest_rank_contract(self) -> None:
        self.assertEqual(reference.percentile(list(range(1, 101)), 50), 51)
        self.assertEqual(reference.percentile(list(range(1, 101)), 99), 100)


if __name__ == "__main__":
    unittest.main()
