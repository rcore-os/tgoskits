"""Exercise the deployed launcher protocol without devices or robot motion."""

import hashlib
import io
import os
from pathlib import Path
import select
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import unittest


APP_DIR = Path(__file__).resolve().parents[1]
VERDICT = (
    "[ROBOT_CI] APPLICATION_PASS flow=1 perf_windows=2 processed=600 "
    "elapsed_s=20.00 effective_fps=30.00 min_fps=28.00 wheels=3 "
    "directions=2 ball_seen=1 ball_drive=1 bucket_drive=1 safe_stop=1"
)


def awk_implementations():
    return sorted({
        Path(path).resolve()
        for name in ("awk", "mawk", "gawk", "busybox")
        if (path := shutil.which(name))
    })


class RobotLauncherTests(unittest.TestCase):
    def prepare(self, root, implementation, producer):
        tools = root / "tools"
        tools.mkdir()
        (tools / "awk").symlink_to(implementation)
        for name, body in (
            ("lsusb", 'printf "FAKE_USB_READY\\n"\n'),
            ("sleep", "exit 0\n"),
        ):
            path = tools / name
            path.write_text("#!/bin/sh\n" + body)
            path.chmod(0o755)
        (root / "models").mkdir()
        (root / "models/tennis.rknn").touch()
        (root / "build").mkdir()
        program = root / "build/tennis"
        program.write_text(
            '#!/bin/sh\nif [ "$1" = test-new-arm ]; then exit 0; fi\n'
            + producer
        )
        program.chmod(0o755)
        launcher = root / "run_robot_ci_once.sh"
        launcher.write_bytes((APP_DIR / launcher.name).read_bytes())
        env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"])
        env["MODEL_PATH"] = str(root / "models/tennis.rknn")
        return launcher, env

    def test_logs_arrive_before_application_exit(self):
        implementations = awk_implementations()
        self.assertTrue(implementations, "an awk implementation is required")
        for implementation in implementations:
            with self.subTest(awk=str(implementation)), tempfile.TemporaryDirectory() as tmp:
                # The application cannot exit until the reader observes its
                # first record. Shell printf writes directly to the pipe.
                producer = (
                    "printf 'LIVE_ROBOT_OUTPUT\\n'\nread -r release\n"
                    + "printf '%s\\n' " + shlex.quote(VERDICT) + "\nexit 0\n"
                )
                launcher, env = self.prepare(Path(tmp), implementation, producer)
                with subprocess.Popen(
                    ["sh", str(launcher), "28.0"], env=env,
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                ) as process:
                    try:
                        first = b""
                        # Startup emits two records before application output.
                        # Read bytes, not buffered readline, so select observes
                        # the same data that the test actually consumes.
                        while b"LIVE_ROBOT_OUTPUT\n" not in first:
                            readable, _, _ = select.select([process.stdout], [], [], 2)
                            if not readable:
                                break
                            chunk = os.read(process.stdout.fileno(), 4096)
                            if not chunk:
                                break
                            first += chunk
                    finally:
                        remaining, errors = process.communicate(b"continue\n", timeout=5)
                self.assertIn(b"LIVE_ROBOT_OUTPUT\n", first, errors)
                self.assertEqual(process.returncode, 0, errors)
                self.assertIn(b"[ROBOT_CI] RESULT=PASS attempts=1", first + remaining)

    def test_application_verdict_and_exit_status_are_both_required(self):
        implementations = awk_implementations()
        self.assertTrue(implementations, "an awk implementation is required")
        cases = (
            (VERDICT, 0, True),
            (VERDICT, 7, False),
            ("[PERF] effective=30.00fps", 0, False),
            (VERDICT.replace("effective_fps=30.00", "effective_fps=27.99"), 0, False),
            (VERDICT.replace("min_fps=28.00", "min_fps=12.50"), 0, False),
            (VERDICT.replace("safe_stop=1", "safe_stop=0"), 0, False),
            (VERDICT + "\n" + VERDICT, 0, False),
        )
        for implementation in implementations:
            for verdict, status, accepted in cases:
                with self.subTest(awk=str(implementation), verdict=verdict, status=status):
                    with tempfile.TemporaryDirectory() as tmp:
                        launcher, env = self.prepare(
                            Path(tmp), implementation,
                            "printf '%s\\n' " + shlex.quote(verdict) + f"\nexit {status}\n",
                        )
                        result = subprocess.run(
                            ["sh", str(launcher), "28.0"], env=env,
                            capture_output=True, text=True, timeout=5,
                        )
                    self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
                    self.assertEqual("[ROBOT_CI] RESULT=PASS" in result.stdout, accepted)
                    if not accepted:
                        self.assertIn("[ROBOT_CI] RESULT=FAIL attempts=2", result.stdout)

    def test_package_uses_local_launcher_and_records_its_digest(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            app = root / "apps/robot"
            app.mkdir(parents=True)
            binary = app / "prebuilt/aarch64/build/tennis"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"fixture binary\n")
            for name in ("prepare-package.sh", "run_robot_ci_once.sh"):
                (app / name).write_bytes((APP_DIR / name).read_bytes())
            source_archive = root / "source.tar.gz"
            source_files = {
                "3rd/rknpu2/Linux/aarch64/librknnrt.so": b"fixture library\n",
                "config/fixture.txt": b"fixture config\n",
                "models/tennis.rknn": b"fixture model\n",
                "run_robot_ci_once.sh": b"upstream launcher must be replaced\n",
            }
            for name in (
                "run_bucket_place_demo.sh", "run_lekiwi_full.sh", "run_lekiwi_loop.sh",
                "run_lekiwi_test.sh", "run_vision_once.sh",
            ):
                source_files[name] = b"#!/bin/sh\nexit 0\n"
            with tarfile.open(source_archive, "w:gz") as archive:
                for name, data in source_files.items():
                    info = tarfile.TarInfo("source/" + name)
                    info.size = len(data)
                    archive.addfile(info, io.BytesIO(data))
            source_digest = hashlib.sha256(source_archive.read_bytes()).hexdigest()
            binary_digest = hashlib.sha256(binary.read_bytes()).hexdigest()
            (app / "source.env").write_text(
                'AKA_RK3588_REPOSITORY="https://example.invalid/fixture"\n'
                'AKA_RK3588_COMMIT="fixture"\n'
                f'AKA_RK3588_SOURCE_SHA256="{source_digest}"\n'
                f'AKA_RK3588_BINARY_SHA256="{binary_digest}"\n'
            )
            output = root / "output"
            result = subprocess.run(
                ["bash", str(app / "prepare-package.sh")],
                env=dict(os.environ, AKA_RK3588_SOURCE_ARCHIVE=str(source_archive),
                         AKA_RK3588_OUTPUT_DIR=str(output)),
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            with tarfile.open(output / "aka-rk3588.tar.gz") as archive:
                launcher = archive.extractfile("aka-rk3588/run_robot_ci_once.sh").read()
                self.assertEqual(launcher, (app / "run_robot_ci_once.sh").read_bytes())
                metadata = archive.extractfile("aka-rk3588/SOURCE").read().decode()
                self.assertIn("launcher_sha256=" + hashlib.sha256(launcher).hexdigest(), metadata)
                self.assertEqual(archive.extractfile("aka-rk3588/build/tennis").read(), binary.read_bytes())


if __name__ == "__main__":
    unittest.main()
