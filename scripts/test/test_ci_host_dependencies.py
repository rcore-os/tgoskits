#!/usr/bin/env python3

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parents[2]


class PrebuiltHostDependenciesTests(unittest.TestCase):
    def test_missing_go_compiler_never_downloads_a_host_toolchain(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = root / "bin"
            binaries.mkdir()
            for tool in ("uname", "mkdir"):
                (binaries / tool).symlink_to(shutil.which(tool))
            calls = root / "downloads"
            curl = binaries / "curl"
            curl.write_text(
                '#!/bin/bash\necho download >> "$INSTALLER_CALLS"\nexit 91\n',
                encoding="utf-8",
            )
            curl.chmod(0o755)
            result = subprocess.run(
                ["/bin/bash", str(WORKSPACE_ROOT / "apps/starry/go-lang/prebuild.sh")],
                env={
                    **os.environ,
                    "PATH": str(binaries),
                    "HOME": str(root),
                    "GO_CARPET_CACHE": str(root / "cache"),
                    "STARRY_APP_DIR": str(WORKSPACE_ROOT / "apps/starry/go-lang"),
                    "STARRY_ARCH": "x86_64",
                    "STARRY_OVERLAY_DIR": str(root / "overlay"),
                    "TGOSKITS_PREBUILT_CONTAINER": "1",
                    "INSTALLER_CALLS": str(calls),
                },
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(calls.exists(), "prepared Go must not be downloaded at runtime")
            self.assertIn("container", result.stderr.lower())

    def test_missing_rust_tools_never_install_targets_or_global_cargo_tools(self) -> None:
        for target_installed in (False, True):
            with self.subTest(target_installed=target_installed), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                binaries = root / "bin"
                binaries.mkdir()
                calls = root / "install-calls"
                for tool in ("grep", "basename"):
                    (binaries / tool).symlink_to(shutil.which(tool))
                rustup = binaries / "rustup"
                rustup.write_text(
                    '#!/bin/bash\ncase "$*" in\n'
                    '  "show active-toolchain") echo fixture-nightly ;;\n'
                    '  "target list --installed"*)\n'
                    '    if [ "$TARGET_INSTALLED" = 1 ]; then echo x86_64-unknown-linux-musl; fi ;;\n'
                    '  *) echo "rustup $*" >> "$INSTALLER_CALLS"; exit 91 ;;\n'
                    'esac\n',
                    encoding="utf-8",
                )
                cargo = binaries / "cargo"
                cargo.write_text(
                    '#!/bin/bash\necho "cargo $*" >> "$INSTALLER_CALLS"\nexit 92\n',
                    encoding="utf-8",
                )
                rustup.chmod(0o755)
                cargo.chmod(0o755)
                result = subprocess.run(
                    ["/bin/bash", str(WORKSPACE_ROOT / "apps/starry/ebpf/upb/prebuild.sh")],
                    env={
                        **os.environ,
                        "PATH": str(binaries),
                        "HOME": str(root),
                        "STARRY_APP_DIR": str(WORKSPACE_ROOT / "apps/starry/ebpf/upb"),
                        "STARRY_WORKSPACE": str(root),
                        "STARRY_OVERLAY_DIR": str(root / "overlay"),
                        "TGOSKITS_PREBUILT_CONTAINER": "1",
                        "TARGET_INSTALLED": "1" if target_installed else "0",
                        "INSTALLER_CALLS": str(calls),
                    },
                    capture_output=True,
                    text=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(calls.exists(), "prepared Rust tools must not be installed at runtime")
                self.assertIn("container", result.stderr.lower())

    def test_missing_host_dependency_fails_without_running_a_package_manager(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = root / "bin"
            binaries.mkdir()
            calls = root / "package-manager-calls"
            installer = binaries / "apt-get"
            installer.write_text(
                '#!/bin/bash\nprintf "%s\\n" "$*" >> "$INSTALLER_CALLS"\nexit 91\n',
                encoding="utf-8",
            )
            installer.chmod(0o755)
            environment = {
                **os.environ,
                "PATH": str(binaries),
                "STARRY_APP_DIR": str(WORKSPACE_ROOT / "apps/starry/pip"),
                "STARRY_ROOTFS": str(root / "rootfs.img"),
                "STARRY_STAGING_ROOT": str(root / "staging"),
                "STARRY_OVERLAY_DIR": str(root / "overlay"),
                "INSTALLER_CALLS": str(calls),
            }
            script = WORKSPACE_ROOT / "apps/starry/pip/prebuild.sh"
            container = subprocess.run(
                ["/bin/bash", str(script)],
                env={**environment, "TGOSKITS_PREBUILT_CONTAINER": "1"},
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(container.returncode, 0)
            self.assertFalse(calls.exists(), "a prepared container must never try apt-get")
            self.assertIn("container", container.stderr.lower())

            host = subprocess.run(
                ["/bin/bash", str(script)],
                env={**environment, "TGOSKITS_PREBUILT_CONTAINER": ""},
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(host.returncode, 0)
            self.assertEqual(calls.read_text().splitlines(), ["update"])


if __name__ == "__main__":
    unittest.main()
