"""Contract tests for the independent StarryOS nixosTest VM launcher."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("launch-vm.py")
SPEC = importlib.util.spec_from_file_location("starry_launch_vm", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
LAUNCH_VM = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LAUNCH_VM)


class LaunchVmContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.root = Path(self.tempdir.name)
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()
        self.config = self.workspace / "qemu-x86_64.toml"
        self.config.write_text(
            """
args = [
  "-machine",
  "q35",
  "-nographic",
  "-drive",
  "id=disk0,if=none,format=raw,file=${workspace}/tmp/axbuild/rootfs/rootfs-x86_64-nixos.img",
]
uefi = true
to_bin = true
""",
            encoding="utf-8",
        )

    def test_translates_canonical_toml_arguments(self) -> None:
        config = LAUNCH_VM.load_qemu_config(self.config)
        self.assertEqual(
            config.args[:3],
            ["-machine", "q35", "-nographic"],
        )
        self.assertTrue(config.uefi)
        self.assertTrue(config.to_bin)

    def test_replaces_only_the_managed_rootfs(self) -> None:
        config = LAUNCH_VM.load_qemu_config(self.config)
        overlay = self.root / "rootfs-overlay.qcow2"
        args = LAUNCH_VM.replace_managed_rootfs(
            config.args,
            overlay,
        )

        self.assertEqual(args.count("-drive"), 1)
        self.assertIn(
            f"id=disk0,if=none,format=qcow2,file={overlay}",
            args,
        )
        self.assertFalse(any("${workspace}" in arg for arg in args))

    def test_rejects_unknown_workspace_substitution(self) -> None:
        config = LAUNCH_VM.load_qemu_config(self.config)
        config.args.extend(["-drive", "file=${workspace}/unexpected.img"])

        with self.assertRaisesRegex(ValueError, "workspace"):
            LAUNCH_VM.replace_managed_rootfs(
                config.args,
                self.root / "overlay.qcow2",
            )

    def test_rejects_conflicting_disk_definition(self) -> None:
        config = LAUNCH_VM.load_qemu_config(self.config)
        config.args.extend(["-drive", "id=disk0,if=none,file=/tmp/conflict.img"])

        with self.assertRaisesRegex(ValueError, "disk0"):
            LAUNCH_VM.replace_managed_rootfs(
                config.args,
                self.root / "overlay.qcow2",
            )

    def test_rejects_existing_overlay(self) -> None:
        overlay = self.root / "rootfs-overlay.qcow2"
        overlay.write_bytes(b"stale")

        with self.assertRaisesRegex(FileExistsError, "overlay"):
            LAUNCH_VM.ensure_overlay_absent(overlay)

    def test_preserves_driver_argument_order(self) -> None:
        driver_args = [
            "-qmp",
            "unix:/tmp/qmp.sock,server=on,wait=off",
            "-monitor",
            "unix:/tmp/monitor.sock,server=on,wait=off",
            "-serial",
            "file:/tmp/serial.log",
            "-no-reboot",
        ]
        command = LAUNCH_VM.build_qemu_command(
            Path("/nix/store/qemu/bin/qemu-system-x86_64"),
            ["-machine", "q35"],
            ["-drive", "if=pflash,file=/tmp/code.fd"],
            driver_args,
        )

        self.assertEqual(command[-len(driver_args) :], driver_args)

    def test_final_plan_executes_qemu_directly(self) -> None:
        plan = LAUNCH_VM.LaunchPlan(
            qemu=Path("/nix/store/qemu/bin/qemu-system-x86_64"),
            args=("-machine", "q35", "-no-reboot"),
        )

        self.assertEqual(
            plan.exec_argv(),
            [
                "/nix/store/qemu/bin/qemu-system-x86_64",
                "-machine",
                "q35",
                "-no-reboot",
            ],
        )


if __name__ == "__main__":
    unittest.main()
