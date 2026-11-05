#!/usr/bin/env python3
"""Exercise the real receiver over HTTP, including restoration of used images."""

import contextlib
import errno
import functools
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import tarfile
import threading
import unittest
from unittest.mock import patch


APP = Path(__file__).resolve().parents[1]
REPO = APP.parents[2]


class Handler(SimpleHTTPRequestHandler):
    requests = []

    def do_GET(self):
        self.requests.append(self.path)
        super().do_GET()

    def log_message(self, *args):
        pass


class ImageTransfer(unittest.TestCase):
    def test_source_snapshot_contains_only_the_recorded_commit(self):
        prepare_source = runpy.run_path(str(APP / "prepare_image.py"))["prepare_source"]
        (REPO / "tmp").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="source-snapshot-", dir=REPO / "tmp") as temporary:
            root = Path(temporary)
            repo = root / "repo"
            repo.mkdir()
            git = ["git", "-C", str(repo)]
            subprocess.run(git + ["init", "-q"], check=True)
            (repo / "source.rs").write_text("committed source\n")
            subprocess.run(git + ["add", "source.rs"], check=True)
            subprocess.run(git + ["-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                                 "commit", "-qm", "initial"], check=True)
            (repo / "private-untracked").write_text("must never leave the host")
            archive = root / "source.tar"
            commit = prepare_source(repo, archive)
            expected = subprocess.check_output(git + ["rev-parse", "HEAD"], text=True).strip()
            self.assertEqual(commit, expected)
            with tarfile.open(archive) as snapshot:
                self.assertEqual(snapshot.getnames(), ["source.rs"])
                self.assertEqual(snapshot.extractfile("source.rs").read(), b"committed source\n")
            first = archive.read_bytes()
            prepare_source(repo, archive)
            self.assertEqual(archive.read_bytes(), first)
            (repo / "source.rs").write_text("uncommitted change\n")
            with self.assertRaises(subprocess.CalledProcessError):
                prepare_source(repo, archive)
            self.assertEqual(archive.read_bytes(), first)

    def test_download_restore_and_reject_invalid_image(self):
        (REPO / "tmp").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="image-transfer-", dir=REPO / "tmp") as temporary:
            root = Path(temporary)
            assets = root / "assets"
            assets.mkdir()
            base = root / "board"
            (base / "session").mkdir(parents=True)
            payloads = [b"first\0" * 250000, b"second\0" * 180000]
            parts = []
            for index, data in enumerate(payloads):
                name = "image.part-{:03d}".format(index)
                (assets / name).write_bytes(data)
                parts.append(dict(name=name, size=len(data), sha256=hashlib.sha256(data).hexdigest()))
            expected = b"".join(payloads)
            manifest = dict(format=1, size=len(expected),
                            sha256=hashlib.sha256(expected).hexdigest(), parts=parts)
            manifest_path = base / "session/manifest.json"
            manifest_path.write_text(json.dumps(manifest))
            server = ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(assets)))
            worker = threading.Thread(target=server.serve_forever)
            worker.start()
            try:
                command = [sys.executable, str(APP / "receive_image.py"),
                           "http://127.0.0.1:" + str(server.server_port), str(base)]
                result = subprocess.run(command, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((base / "image.ext4").read_bytes(), expected)
                self.assertIn("selfbuild_image=verified", result.stdout)
                Handler.requests.clear()
                result = subprocess.run(command, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(Handler.requests, [])
                # A mounted build can change both image bytes and file length.
                with (base / "image.ext4").open("r+b") as image:
                    image.seek(len(payloads[0]) + 31)
                    image.write(b"changed by guest")
                    image.seek(0, 2)
                    image.write(b"stale tail")
                result = subprocess.run(command, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(Handler.requests, ["/image.part-001"])
                self.assertEqual((base / "image.ext4").read_bytes(), expected)
                receive = runpy.run_path(str(APP / "receive_image.py"))["receive"]
                for reuse in (True, False):
                    if not reuse:
                        (base / "image.ext4").unlink()
                    Handler.requests.clear()
                    output = io.StringIO()
                    with contextlib.redirect_stdout(output), patch("time.sleep"), patch(
                            "os.fsync", side_effect=OSError(errno.ENOSPC, "injected storage failure")) as sync:
                        with self.assertRaises(OSError) as failure:
                            receive(command[-2], base)
                    self.assertEqual(failure.exception.errno, errno.ENOSPC)
                    self.assertNotIn("selfbuild_image_part=", output.getvalue())
                    self.assertNotIn("selfbuild_image=verified", output.getvalue())
                    self.assertEqual(sync.call_count, 3)
                    self.assertEqual(Handler.requests, ["/image.part-000"] * (2 if reuse else 3))
                (base / "image.ext4").unlink()
                Handler.requests.clear()
                real_sync = os.fsync
                failures = [OSError(errno.ENOSPC, "temporary storage failure")] * 2

                def recover_writeback(fd):
                    if failures:
                        raise failures.pop()
                    real_sync(fd)

                output = io.StringIO()
                with contextlib.redirect_stdout(output), patch("time.sleep"), patch(
                        "os.fsync", side_effect=recover_writeback):
                    receive(command[-2], base)
                self.assertEqual(Handler.requests, ["/image.part-000"] * 3 + ["/image.part-001"])
                self.assertEqual(output.getvalue().count("selfbuild_image_retry="), 2)
                self.assertEqual(output.getvalue().count("selfbuild_image_part=image.part-000"), 1)
                self.assertIn("selfbuild_image=verified", output.getvalue())
                self.assertEqual((base / "image.ext4").read_bytes(), expected)
                manifest["sha256"] = "0" * 64
                manifest_path.write_text(json.dumps(manifest))
                result = subprocess.run(command, text=True, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("selfbuild_image=verified", result.stdout)
                manifest["parts"][0]["name"] = "../outside"
                manifest_path.write_text(json.dumps(manifest))
                Handler.requests.clear()
                result = subprocess.run(command, text=True, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(Handler.requests, [])
                self.assertEqual((base / "image.ext4").read_bytes(), expected)
            finally:
                server.shutdown()
                worker.join()
                server.server_close()


if __name__ == "__main__":
    unittest.main()
