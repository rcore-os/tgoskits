"""Contract tests for the independent StarryOS nixosTest VM launcher."""

from __future__ import annotations

import importlib.util
import shutil
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

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
  "id=disk0,if=none,format=raw,file=${workspace}/target/axbuild/rootfs/rootfs-x86_64-nixos.img",
]
uefi = true
to_bin = true
""",
            encoding="utf-8",
        )

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

    @unittest.skipUnless(shutil.which("qemu-img"), "qemu-img is required")
    def test_stages_configured_command_line_on_boot_volume(self) -> None:
        kernel = self.root / "kernel.efi"
        kernel.write_bytes(b"kernel fixture")
        rootfs = self.root / "rootfs.img"
        with rootfs.open("wb") as image:
            image.truncate(1024 * 1024)
        original_config = self.config.read_text(encoding="utf-8")

        for index, cmdline in enumerate([None, 'init=/custom-init LABEL="test root"']):
            with self.subTest(cmdline=cmdline):
                config = original_config
                if cmdline is not None:
                    config += f"\ncmdline = '{cmdline}'\n"
                self.config.write_text(config, encoding="utf-8")
                run_dir = self.root / f"run-{index}"
                args = SimpleNamespace(
                    qemu=kernel,
                    qemu_img=Path(shutil.which("qemu-img")),
                    ovmf_code=kernel,
                    ovmf_vars=kernel,
                    kernel=kernel,
                    rootfs=rootfs,
                    run_dir=run_dir,
                    qemu_config=self.config,
                )
                plan = LAUNCH_VM.prepare_launch_plan(args, ["-no-reboot"])
                esp = run_dir / "starry-nixos.esp"
                self.assertIn(f"format=raw,file=fat:rw:{esp}", plan.args)
                self.assertEqual(plan.args[-1], "-no-reboot")
                command_file = esp / "EFI" / "BOOT" / "cmdline.txt"
                if cmdline is None:
                    self.assertFalse(command_file.exists())
                else:
                    self.assertTrue(command_file.is_file(), "boot volume lacks cmdline.txt")
                    self.assertEqual(command_file.read_bytes(), cmdline.encode("utf-8"))

    def test_rejects_non_string_command_line(self) -> None:
        original_config = self.config.read_text(encoding="utf-8")
        for value in ["123", "false", "[]"]:
            with self.subTest(value=value):
                self.config.write_text(original_config + f"\ncmdline = {value}\n")
                with self.assertRaisesRegex(ValueError, "cmdline"):
                    LAUNCH_VM.load_qemu_config(self.config)

if __name__ == "__main__":
    unittest.main()
