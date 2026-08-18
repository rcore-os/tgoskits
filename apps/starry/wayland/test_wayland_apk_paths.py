import os
import tempfile
import unittest

from wayland_apk_paths import cache_path, package_filename


class WaylandApkPathTests(unittest.TestCase):
    def test_valid_package_filename_stays_a_single_component(self):
        self.assertEqual(package_filename("weston", "14.0.2-r0"), "weston-14.0.2-r0.apk")

    def test_rejects_package_and_version_path_traversal(self):
        for value in ("../../outside", "/tmp/outside", "weston/cache", ".", ".."):
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    package_filename(value, "1-r0")
                with self.assertRaises(ValueError):
                    package_filename("weston", value)

    def test_rejects_an_existing_symlink_outside_the_cache_root(self):
        with tempfile.TemporaryDirectory() as root, tempfile.TemporaryDirectory() as outside:
            for filename in ("weston-1-r0.apk", "weston-1-r0.apk.tmp"):
                with self.subTest(filename=filename):
                    link = os.path.join(root, filename)
                    os.symlink(os.path.join(outside, "victim.apk"), link)
                    with self.assertRaises(ValueError):
                        cache_path(root, filename)
                    os.unlink(link)

    def test_rejects_a_cache_target_outside_its_root(self):
        with tempfile.TemporaryDirectory() as parent:
            root = os.path.join(parent, "cache")
            os.mkdir(root)
            outside = os.path.join(parent, "outside.apk")
            with self.assertRaises(ValueError):
                cache_path(root, "../outside.apk")
            self.assertFalse(os.path.exists(outside))

    def test_returns_a_path_under_the_cache_root(self):
        with tempfile.TemporaryDirectory() as root:
            target = cache_path(root, package_filename("weston", "1-r0"))
            self.assertEqual(os.path.commonpath((os.path.realpath(root), target)), os.path.realpath(root))


if __name__ == "__main__":
    unittest.main()
