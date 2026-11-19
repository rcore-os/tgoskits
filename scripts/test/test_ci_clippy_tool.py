"""The distributed executable must be usable and invalid payloads rejected."""
import io
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

import ci_clippy_tool as tool


class ClippyToolDistributionTests(unittest.TestCase):
    def test_roundtrip_preserves_source_and_rejects_corruption_or_unsafe_members(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "original"
            shutil.copyfile(shutil.which("true"), source)
            original = tool.sha256(source)
            tool.pack(source, root / "artifact")
            destination = root / "bin/tg-xtask"
            tool.unpack(root / "artifact", destination)
            self.assertEqual(subprocess.run([str(destination)], check=False).returncode, 0)
            self.assertEqual(tool.sha256(source), original)
            accepted = tool.sha256(destination)
            archive = root / "artifact" / tool.ARCHIVE
            archive.write_bytes(archive.read_bytes() + b"corruption")
            with self.assertRaisesRegex(ValueError, "checksum"):
                tool.unpack(root / "artifact", destination)
            self.assertEqual(tool.sha256(destination), accepted)
            for name in ["../tg-xtask", "tg-xtask"]:
                with self.subTest(name=name):
                    with tarfile.open(archive, "w:xz", preset=1) as tar:
                        member = tarfile.TarInfo(name)
                        if name == "tg-xtask":
                            member.type = tarfile.SYMTYPE
                            member.linkname = "../original"
                            tar.addfile(member)
                        else:
                            member.size = 4
                            tar.addfile(member, io.BytesIO(b"fake"))
                    (archive.parent / tool.CHECKSUM).write_text(f"{tool.sha256(archive)}  {tool.ARCHIVE}\n")
                    with self.assertRaisesRegex(ValueError, "regular file"):
                        tool.unpack(archive.parent, destination)
                    self.assertEqual(tool.sha256(destination), accepted)


if __name__ == "__main__":
    unittest.main()
