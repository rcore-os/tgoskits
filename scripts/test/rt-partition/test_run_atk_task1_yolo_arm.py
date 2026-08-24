#!/usr/bin/env python3
"""Tests for the ATK sustained YOLO board runner configuration."""

import importlib.util
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[2] / "board/run-atk-task1-yolo-arm.py"
SPEC = importlib.util.spec_from_file_location("run_atk_task1_yolo_arm", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
RUNNER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RUNNER
SPEC.loader.exec_module(RUNNER)


class RunAtkTask1YoloArmTest(unittest.TestCase):
    def test_guest_setup_allows_physical_board_boot_to_finish(self):
        console = mock.Mock()

        RUNNER.prepare_starry_guest(console)

        console.expect.assert_called_once_with(RUNNER.GUEST_PROMPT, 60)

    def test_console_drain_logs_serial_without_mirroring_bulk_output(self):
        class FakeSerial:
            def __init__(self, *args, **kwargs):
                self.chunks = [b"sequence,timestamp_ns\n0,10000000\n", b""]

            def read(self, _size):
                return self.chunks.pop(0) if self.chunks else b""

            def close(self):
                pass

        class FakeStdout:
            def __init__(self):
                self.buffer = io.BytesIO()

        with tempfile.TemporaryDirectory() as directory:
            log_path = Path(directory) / "console.log"
            stdout = FakeStdout()
            with mock.patch.object(RUNNER.serial, "Serial", FakeSerial), mock.patch.object(
                RUNNER.sys, "stdout", stdout
            ):
                console = RUNNER.Console("/dev/fake", 1_500_000, log_path)
                console.drain(0.01)
                console.close()

            self.assertEqual(
                log_path.read_bytes(), b"sequence,timestamp_ns\n0,10000000\n"
            )
            self.assertEqual(stdout.buffer.getvalue(), b"")

    def test_zero_runtime_preserves_single_inference_smoke_mode(self):
        config = make_config(runtime_seconds=0, expected_inferences=1)

        self.assertEqual(config.model_mode, "model-only")
        self.assertEqual(config.periodic_timeout_seconds, 195)

    def test_sustained_runtime_uses_continuous_model_loop_and_scaled_timeout(self):
        config = make_config(
            runtime_seconds=180,
            expected_inferences=50,
            expected_samples=18_000,
        )

        self.assertEqual(config.model_mode, "model-loop")
        self.assertEqual(config.periodic_timeout_seconds, 1080)

    def test_sustained_model_collection_observes_progress_after_sampling(self):
        console = mock.Mock()
        config = make_config(runtime_seconds=180, expected_inferences=50)

        RUNNER.collect_model_results(console, config)

        self.assertEqual(
            console.method_calls,
            [
                mock.call.command(
                    "vm console 1", rb"Attached VM\[1\] console", 30
                ),
                mock.call.expect_count(RUNNER.INFERENCE_COMPLETE, 50, 180),
                mock.call.clear_match_window(),
                mock.call.expect(
                    RUNNER.INFERENCE_COMPLETE,
                    RUNNER.POST_SAMPLING_INFERENCE_TIMEOUT_SECONDS,
                ),
                mock.call.clear_match_window(),
                mock.call.raw(b"\x03"),
                mock.call.expect(RUNNER.GUEST_PROMPT, 30),
                mock.call.detach(),
            ],
        )

    def test_periodic_rows_are_dumped_only_after_an_explicit_command(self):
        console = mock.Mock()
        config = make_config(expected_samples=18_000)

        RUNNER.collect_periodic_results(console, config)

        self.assertEqual(
            console.method_calls,
            [
                mock.call.command(
                    "vm console 2", rb"Attached VM\[2\] console", 30
                ),
                mock.call.clear_match_window(),
                mock.call.raw(RUNNER.PERIODIC_DUMP_COMMAND),
                mock.call.expect(
                    rb"PERIODIC LATENCY COMPLETE samples=18000\b",
                    config.periodic_timeout_seconds,
                ),
            ],
        )

    def test_zephyr_rows_are_dumped_after_the_workload_is_stopped(self):
        console = mock.Mock()
        config = make_config(periodic_guest="zephyr")

        RUNNER.collect_periodic_results(console, config)

        self.assertEqual(
            console.method_calls,
            [
                mock.call.command(
                    "vm console 2", rb"Attached VM\[2\] console", 30
                ),
                mock.call.clear_match_window(),
                mock.call.raw(RUNNER.PERIODIC_DUMP_COMMAND),
                mock.call.expect(
                    rb"PERIODIC LATENCY COMPLETE samples=300\b",
                    config.periodic_timeout_seconds,
                ),
            ],
        )

    def test_metadata_records_artifact_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "guest.bin"
            artifact.write_bytes(b"guest")
            config = make_config(artifacts=(artifact,), root=root)

            RUNNER.write_metadata(config, "complete", elapsed_seconds=1.25)

            metadata = config.metadata_path.read_text()
            self.assertIn("status=complete", metadata)
            self.assertIn("periodic_timeout_seconds=195", metadata)
            self.assertIn(
                "artifact_sha256=84983c60f7daadc1cb8698621f802c0d9f9a3c3c295c810748fb048115c186ec",
                metadata,
            )


def make_config(
    *,
    runtime_seconds=0,
    expected_inferences=1,
    expected_samples=300,
    artifacts=(),
    root=Path("/tmp"),
    periodic_guest="rtthread",
):
    return RUNNER.RunConfig(
        log_path=root / "run.log",
        metadata_path=root / "run.metadata.txt",
        port="/dev/null",
        baud=1_500_000,
        scheduler="rr",
        runtime_seconds=runtime_seconds,
        expected_inferences=expected_inferences,
        expected_samples=expected_samples,
        period_ms=10,
        completion_grace_seconds=180,
        artifacts=artifacts,
        periodic_guest=periodic_guest,
    )


if __name__ == "__main__":
    unittest.main()
