#!/usr/bin/env python3
"""Run the guest provisioning script with real apk-tools in an offline sandbox.

Set APK_TEST_TOOL to an apk.static executable and APK_TEST_FIXTURES to a
folder containing official alpine-keys-*.apk, alpine-baselayout-data-*.apk,
and repo/x86_64/APKINDEX.tar.gz downloaded over authenticated HTTPS.
The host needs bubblewrap and a static busybox. No network is used by tests.
"""

import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
import zlib


class ProvisionAuthenticationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tool = Path(os.environ["APK_TEST_TOOL"]).resolve()
        cls.fixtures = Path(os.environ["APK_TEST_FIXTURES"]).resolve()
        cls.bwrap = shutil.which("bwrap")
        cls.busybox = shutil.which("busybox")
        if not cls.bwrap or not cls.busybox or not cls.tool.is_file():
            raise RuntimeError("real apk.static, bubblewrap and static busybox are required")
        source = Path(__file__).with_name("run-hvf.sh").read_text()
        cls.script = source.split("<<'SCRIPT'\n", 1)[1].split("\nSCRIPT", 1)[0]
        cls.package = next(cls.fixtures.glob("alpine-baselayout-data-*.apk"))
        cls.keys = next(cls.fixtures.glob("alpine-keys-*.apk"))

    def run_provision(self, mutation=None):
        with tempfile.TemporaryDirectory(prefix="wayland-apk-test-") as temporary:
            root = Path(temporary)
            for directory in ("bin", "etc/apk/keys", "tmp"):
                (root / directory).mkdir(parents=True)
            shutil.copy(self.busybox, root / "bin/busybox")
            for name in ("sh", "xargs", "touch"):
                (root / "bin" / name).symlink_to("busybox")
            shutil.copy(self.tool, root / "bin/apk.real")
            apk_wrapper = root / "bin/apk"
            apk_wrapper.write_text(
                '#!/bin/sh\nprintf "%s\\n" "$*" >> /apk-invocations\n'
                'exec /bin/apk.real "$@"\n'
            )
            apk_wrapper.chmod(0o755)
            with tarfile.open(self.keys) as archive:
                for member in archive.getmembers():
                    if member.name.startswith("etc/apk/keys/") and member.isfile():
                        archive.extract(member, root, filter="data")
            sandbox = [
                self.bwrap, "--unshare-all", "--uid", "0", "--gid", "0",
                "--die-with-parent", "--bind", str(root), "/", "--dev", "/dev",
                "--setenv", "PATH", "/bin",
            ]
            initialization = subprocess.run(
                sandbox + ["/bin/apk.real", "--initdb", "--no-network", "add"],
                capture_output=True, text=True,
            )
            self.assertEqual(initialization.returncode, 0, initialization.stderr)
            cache = root / "usr/local/wayland-apks"
            repo = cache / "main/x86_64"
            repo.mkdir(parents=True)
            shutil.copy(self.fixtures / "repo/x86_64/APKINDEX.tar.gz", repo)
            package = repo / self.package.name
            package.write_bytes(self.package.read_bytes())
            if mutation == "unsigned":
                decompressor = zlib.decompressobj(31)
                decompressor.decompress(package.read_bytes())
                package.write_bytes(decompressor.unused_data)
            elif mutation == "tampered":
                data = bytearray(package.read_bytes())
                data[-20] ^= 1
                package.write_bytes(data)
            elif mutation == "unsigned-index":
                other_repo = cache / "community/x86_64"
                other_repo.mkdir(parents=True)
                shutil.copy(repo / "APKINDEX.tar.gz", other_repo)
                index = other_repo / "APKINDEX.tar.gz"
                decompressor = zlib.decompressobj(31)
                decompressor.decompress(index.read_bytes())
                index.write_bytes(decompressor.unused_data)
            elif mutation == "index-mismatch":
                package.write_bytes(self.keys.read_bytes())
            elif mutation == "untrusted":
                shutil.rmtree(root / "etc/apk/keys")
                (root / "etc/apk/keys").mkdir()
            (cache / "repositories").write_text(
                "/usr/local/wayland-apks/main\n"
                + ("/usr/local/wayland-apks/community\n" if mutation == "unsigned-index" else "")
            )
            version = self.package.name.removeprefix("alpine-baselayout-data-").removesuffix(".apk")
            selection = "alpine-baselayout-data=" + version
            (cache / "install.list").write_text(selection + "\n")
            (root / "provision.sh").write_text(self.script)
            result = subprocess.run(
                sandbox + ["/bin/sh", "/provision.sh"], capture_output=True, text=True,
            )
            output = result.stdout + result.stderr
            marker = (root / ".wayland-provisioned").exists()
            installed = (root / "etc/hosts").exists()
            calls_path = root / "apk-invocations"
            calls = calls_path.read_text().splitlines() if calls_path.exists() else []
            if mutation:
                self.assertNotEqual(result.returncode, 0, output)
                self.assertFalse(marker, output)
                self.assertFalse(installed, output)
                self.assertTrue(calls, output)
                if mutation != "index-mismatch":
                    self.assertFalse(any("add" in call.split() for call in calls), output)
            else:
                self.assertEqual(result.returncode, 0, output)
                self.assertTrue(marker, output)
                self.assertTrue(installed, output)
                self.assertIn("PROVISION_DONE", output)

    def test_authenticated_packages_are_installed_and_invalid_packages_abort(self):
        self.run_provision()
        for mutation in ("unsigned", "tampered", "untrusted", "index-mismatch", "unsigned-index"):
            with self.subTest(mutation=mutation):
                self.run_provision(mutation)


if __name__ == "__main__":
    unittest.main()
