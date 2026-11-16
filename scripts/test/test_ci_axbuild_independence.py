"""The source boundary gate rejects private imports, while allowing owned modules."""
import subprocess
import tempfile
from pathlib import Path
import unittest

from scripts.test.check_axbuild_independence import check_source_boundaries


class AxbuildSourceBoundaryTests(unittest.TestCase):
    def test_cross_package_private_imports_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "--quiet", str(root)], check=True)
            library = root / "scripts/axbuild/src"
            library.mkdir(parents=True)
            (library / "private.rs").write_text("pub fn operation() {}\n")
            consumer = root / "build.rs"
            consumer.write_text('#[path = "scripts/axbuild/src/private.rs"]\nmod private;\n')
            with self.assertRaisesRegex(RuntimeError, "includes private axbuild source"):
                check_source_boundaries(root)
            consumer.write_text('include!("scripts/axbuild/src/private.rs");\n')
            with self.assertRaisesRegex(RuntimeError, "includes private axbuild source"):
                check_source_boundaries(root)
            (root / "owned.rs").write_text("pub fn operation() {}\n")
            consumer.write_text('#[path = "owned.rs"]\nmod owned;\n')
            check_source_boundaries(root)


if __name__ == "__main__":
    unittest.main()
