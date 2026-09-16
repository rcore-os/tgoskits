"""Host prefetch contract; run with python3 -m unittest discover in this directory."""

import contextlib
import io
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import urllib.request


SOURCE = Path(__file__).with_name("prebuild.sh").read_text().split("<<'PY'\n", 1)[1].split("\nPY\n", 1)[0]


def index_bytes():
    content = (b"P:weston\nV:1.0-r0\n\n"
               b"P:helper\nV:1.0-r0\ni:weston=1.0-r0\nD:runtime\n\n"
               b"P:runtime\nV:1.0-r0\n\n")
    result = io.BytesIO()
    with tarfile.open(fileobj=result, mode="w:gz") as archive:
        member = tarfile.TarInfo("APKINDEX")
        member.size = len(content)
        archive.addfile(member, io.BytesIO(content))
    return result.getvalue()


class PrefetchTests(unittest.TestCase):
    def run_prefetch(self, root, fetch):
        cache, overlay = root / "cache", root / "overlay"
        cache.mkdir(exist_ok=True)
        overlay.mkdir(exist_ok=True)
        namespace = {"__name__": "__main__"}
        with (
            patch.object(sys, "argv", ["prebuild", "x86_64", "v3.23", str(cache), str(overlay)]),
            patch.dict(os.environ, {"STARRY_WAYLAND_WRITE_INSTALL_LIST": "1",
                                    "STARRY_WAYLAND_INSTALLED_PACKAGES": "",
                                    "STARRY_WAYLAND_EXTRA_APKS": ""}),
            patch.object(urllib.request, "urlopen", side_effect=fetch),
            patch.object(urllib.request.OpenerDirector, "open", side_effect=fetch),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            exec(compile(SOURCE, "prebuild.sh:python", "exec"), namespace)
        return namespace

    def test_https_prefetch_preserves_repository_and_reuses_cache(self):
        index = index_bytes()
        requested = []

        def fetch(url, **kwargs):
            self.assertTrue(url.startswith("https://"), url)
            requested.append(url)
            response = io.BytesIO(index if url.endswith("APKINDEX.tar.gz") else b"package")
            response.headers = {}
            return response

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            namespace = self.run_prefetch(root, fetch)
            overlay = root / "overlay"
            self.assertEqual((overlay / "main/x86_64/APKINDEX.tar.gz").read_bytes(), index)
            for name in ("weston", "helper", "runtime"):
                self.assertEqual((overlay / f"community/x86_64/{name}-1.0-r0.apk").read_bytes(), b"package")
            self.assertEqual((overlay / "install.list").read_text(), "weston=1.0-r0\nhelper=1.0-r0\nruntime=1.0-r0\n")
            self.assertEqual((overlay / "repositories").read_text(),
                             "/usr/local/wayland-apks/main\n/usr/local/wayland-apks/community\n")
            requested.clear()
            self.run_prefetch(root, fetch)
            self.assertTrue(all(url.endswith("APKINDEX.tar.gz") for url in requested))

            # Exercise urllib's redirect chain, without a live external mirror.
            class Redirect(urllib.request.HTTPSHandler):
                handler_order = 0

                def https_open(self, req):
                    headers = {"location": "http://mirror.invalid/payload"}
                    return self.parent.error("http", req, io.BytesIO(), 302, "Found", headers)

                def http_open(self, req):
                    raise AssertionError("plaintext redirect followed")

            opener = namespace["opener"]
            opener.add_handler(Redirect())
            with self.assertRaisesRegex(ValueError, "non-HTTPS redirect"):
                opener.open("https://mirror.invalid/payload")


if __name__ == "__main__":
    unittest.main()
