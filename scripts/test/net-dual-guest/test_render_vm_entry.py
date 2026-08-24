from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("render_vm_entry.py")
SPEC = importlib.util.spec_from_file_location("render_vm_entry", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class RenderVmEntryTests(unittest.TestCase):
    def test_uses_fresh_manifest_entry_instead_of_stale_config_value(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "manifest.toml"
            config = root / "guest.toml"
            output = root / "runtime.toml"
            manifest.write_text('elf_entry = "0xa0001114"\n')
            config.write_text(
                "[kernel]\n"
                "entry_point = 0xA000_117C\n"
                'kernel_path = "${workspace}/guest.bin"\n'
            )

            MODULE.render_vm_entry(manifest, config, output)

            self.assertIn("entry_point = 0xa0001114", output.read_text())
            self.assertNotIn("0xA000_117C", output.read_text())

    def test_rejects_manifest_without_elf_entry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "manifest.toml"
            config = root / "guest.toml"
            manifest.write_text('fault_mode = "none"\n')
            config.write_text("[kernel]\nentry_point = 0xA000_117C\n")

            with self.assertRaisesRegex(ValueError, "elf_entry"):
                MODULE.render_vm_entry(manifest, config, root / "runtime.toml")

    def test_uses_periodic_key_value_manifest_entry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "zephyr-periodic.manifest"
            config = root / "guest.toml"
            output = root / "runtime.toml"
            manifest.write_text(
                "entry_point=0xa0001044\n"
                "board=qemu_cortex_a53\n"
                "extra_overlay=none\n"
            )
            config.write_text("[kernel]\nentry_point = 0xA000_10B4\n")

            MODULE.render_vm_entry(manifest, config, output)

            self.assertIn("entry_point = 0xa0001044", output.read_text())


if __name__ == "__main__":
    unittest.main()
