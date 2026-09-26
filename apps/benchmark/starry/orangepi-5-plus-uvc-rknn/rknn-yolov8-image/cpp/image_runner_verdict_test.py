"""Only complete, successfully released batches may report final success."""
import os
import subprocess
import sys
import unittest

RUNNER = sys.argv.pop(1)


class ImageRunnerVerdictTest(unittest.TestCase):
    def test_complete_batch_and_failure_propagation(self):
        for fault in ("none", "labels", "model", "image", "inference", "release"):
            with self.subTest(fault=fault):
                result = subprocess.run(
                    [RUNNER, "--batch", "model", "labels", "first", "second", "third"],
                    env=dict(os.environ, IMAGE_RUNNER_FAULT=fault),
                    capture_output=True, text=True, timeout=5)
                if fault == "none":
                    self.assertEqual(result.returncode, 0, result.stdout)
                    self.assertEqual(result.stdout.count("UVC_RKNN_IMAGE_PASS images=3"), 1)
                else:
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertNotIn("UVC_RKNN_IMAGE_PASS", result.stdout)
                    self.assertNotIn("Detection Results Summary", result.stdout)

    def test_empty_batch_cannot_pass(self):
        result = subprocess.run([RUNNER, "--batch", "model", "labels"],
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("UVC_RKNN_IMAGE_PASS", result.stdout)

    def test_single_image_cleanup_failure_has_no_success_summary(self):
        result = subprocess.run([RUNNER, "first"],
                                env=dict(os.environ, IMAGE_RUNNER_FAULT="release"),
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("Detection Results Summary", result.stdout)
        self.assertNotIn("UVC_RKNN_IMAGE_DONE", result.stdout)


if __name__ == "__main__":
    unittest.main()
