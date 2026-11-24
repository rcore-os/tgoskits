#!/usr/bin/env python3
"""Exercise guest toolchain recovery with real HTTP, curl and tar."""

import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import io
import os
from pathlib import Path
import shutil
import tarfile
import tempfile
import threading
import unittest
from unittest.mock import patch


PREPARE_PATH = (
    Path(__file__).resolve().parents[2]
    / "virtualization/axvm/tests/guests/virq/prepare.py"
)
SPEC = importlib.util.spec_from_file_location("virq_prepare", PREPARE_PATH)
prepare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prepare)


class GuestToolchainTests(unittest.TestCase):
    def test_recovers_download_and_cache_without_publishing_invalid_archives(self) -> None:
        compiler_bytes = b"fixture compiler\n"
        with io.BytesIO() as buffer:
            with tarfile.open(fileobj=buffer, mode="w:xz") as archive:
                compiler = tarfile.TarInfo(
                    f"{prepare.TOOLCHAIN_ROOT_NAME}/bin/aarch64-none-elf-gcc"
                )
                compiler.size = len(compiler_bytes)
                compiler.mode = 0o755
                archive.addfile(compiler, io.BytesIO(compiler_bytes))
            archive_bytes = buffer.getvalue()

        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary)
            root = cache / "toolchain"
            archive = root / "fixture-toolchain.tar.xz"
            response_bytes = archive_bytes
            requests = 0
            published_during_download = []

            class Handler(BaseHTTPRequestHandler):
                def do_GET(self):
                    nonlocal requests
                    requests += 1
                    published_during_download.append(archive.is_file())
                    if requests == 1:
                        self.close_connection = True
                        return
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(response_bytes)))
                    self.end_headers()
                    self.wfile.write(response_bytes)

                def log_message(self, *_arguments):
                    pass

            with HTTPServer(("127.0.0.1", 0), Handler) as server:
                thread = threading.Thread(target=server.serve_forever, daemon=True)
                thread.start()
                try:
                    url = f"http://127.0.0.1:{server.server_port}/{archive.name}"
                    digest = hashlib.sha256(archive_bytes).hexdigest()
                    with patch.object(prepare, "TOOLCHAIN_ARCHIVE_URL", url), patch.dict(
                        os.environ,
                        {
                            "VIRQ_TOOLCHAIN_SHA256": digest.upper(),
                            "NO_PROXY": "127.0.0.1",
                            "no_proxy": "127.0.0.1",
                        },
                    ):
                        prefix = prepare.install_toolchain(cache)
                        compiler = Path(f"{prefix}gcc")
                        self.assertEqual(compiler.read_bytes(), compiler_bytes)
                        self.assertEqual(archive.read_bytes(), archive_bytes)
                        self.assertEqual(requests, 2)
                        self.assertEqual(published_during_download, [False, False])

                        shutil.rmtree(compiler.parent.parent)
                        self.assertEqual(prepare.install_toolchain(cache), prefix)
                        self.assertEqual(compiler.read_bytes(), compiler_bytes)
                        self.assertEqual(requests, 2, "a valid cached archive needs no HTTP")

                        shutil.rmtree(compiler.parent.parent)
                        archive.write_bytes(archive_bytes[: len(archive_bytes) // 2])
                        self.assertEqual(prepare.install_toolchain(cache), prefix)
                        self.assertEqual(compiler.read_bytes(), compiler_bytes)
                        self.assertEqual(archive.read_bytes(), archive_bytes)
                        self.assertEqual(requests, 3, "a corrupt archive must be downloaded again")
                        self.assertEqual(published_during_download, [False, False, False])

                        shutil.rmtree(compiler.parent.parent)
                        archive.unlink()
                        response_bytes = b"corrupt download"
                        with self.assertRaisesRegex(RuntimeError, "sha256"):
                            prepare.install_toolchain(cache)
                        self.assertEqual(list(root.iterdir()), [], "invalid downloads must leave no cache")
                finally:
                    server.shutdown()
                    thread.join()


if __name__ == "__main__":
    unittest.main()
