from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


MODEL_DIR = Path(__file__).resolve().parents[1] / "model"
SPEC = importlib.util.spec_from_file_location(
    "ivc_export_thermal_ort",
    MODEL_DIR / "export_thermal_ort.py",
)
assert SPEC is not None and SPEC.loader is not None
exporter = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = exporter
SPEC.loader.exec_module(exporter)


class ThermalOrtExportTests(unittest.TestCase):
    def test_regeneration_allows_only_audited_semantic_variants(self) -> None:
        for digest in (
            "3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887",
            "63ccf6753965138723ea88b5e801754c30f5e398567081e0ff9580f57da92ebf",
        ):
            exporter.require_audited_ort_variant(digest)

        with self.assertRaisesRegex(exporter.OrtExportError, "unaudited"):
            exporter.require_audited_ort_variant("0" * 64)

    def test_alternate_regeneration_preserves_existing_canonical_bytes(self) -> None:
        source = exporter.canonical_write_source(
            "63ccf6753965138723ea88b5e801754c30f5e398567081e0ff9580f57da92ebf",
            "3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887",
        )

        self.assertEqual(source, "existing")

    def test_operator_config_removes_output_directory_identity(self) -> None:
        generated = (
            "# Generated from model/s:\n"
            "# - /tmp/build-a/thermal-4x6x1-v1.ort\n"
            'ai.onnx;13;Clip{"inputs": {"0": ["float"]}},Gemm{"inputs": {"0": ["float"]}}\n'
            "com.microsoft;1;FusedGemm\n"
        )

        normalized = exporter.normalize_operator_config(
            generated,
            "thermal-4x6x1-v1.ort",
        )

        self.assertEqual(
            normalized,
            generated.replace(
                "/tmp/build-a/thermal-4x6x1-v1.ort",
                "thermal-4x6x1-v1.ort",
            ),
        )

    def test_operator_config_rejects_unexpected_model_identity(self) -> None:
        generated = (
            "# Generated from model/s:\n"
            "# - /tmp/build-a/another-model.ort\n"
            "ai.onnx;13;Gemm\n"
        )

        with self.assertRaisesRegex(exporter.OrtExportError, "model identity"):
            exporter.normalize_operator_config(
                generated,
                "thermal-4x6x1-v1.ort",
            )


if __name__ == "__main__":
    unittest.main()
