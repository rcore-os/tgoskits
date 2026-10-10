#!/usr/bin/env python3
"""Check instruction rewriting against a linked ELF, including matching data."""
import pathlib
import subprocess
import sys
import tempfile
import unittest

PREPARE = pathlib.Path(__file__).resolve().parents[1] / "prepare-runtime.py"


class RuntimeTest(unittest.TestCase):
    def test_preserves_data_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            source, binary = root / "fixture.S", root / "fixture"
            source.write_text(""".text
.globl _start
_start:
  .2byte 0x0001
  fence.tso
  ret
.section .rodata
  .4byte 0x8330000f
""")
            subprocess.run(["clang", "--target=riscv64-linux-gnu", "-nostdlib",
                            "-fuse-ld=lld", "-Wl,-e,_start", str(source), "-o", str(binary)], check=True)
            original = binary.read_bytes()
            subprocess.run([sys.executable, str(PREPARE), str(binary)], check=True)
            prepared = binary.read_bytes()
            self.assertEqual(len(prepared), len(original))
            differences = [(a, b) for a, b in zip(original, prepared) if a != b]
            self.assertEqual(differences, [(0x83, 0x03)])
            self.assertEqual(original.count(b"\x0f\x00\x30\x83"), 2)
            self.assertEqual(prepared.count(b"\x0f\x00\x30\x83"), 1)
            subprocess.run([sys.executable, str(PREPARE), str(binary)], check=True)
            self.assertEqual(binary.read_bytes(), prepared)
            wrong_arch = bytearray(prepared)
            wrong_arch[18:20] = (62).to_bytes(2, "little")
            binary.write_bytes(wrong_arch)
            result = subprocess.run([sys.executable, str(PREPARE), str(binary)], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(binary.read_bytes(), wrong_arch)


if __name__ == "__main__":
    unittest.main()
