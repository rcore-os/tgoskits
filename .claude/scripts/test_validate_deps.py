"""Unit tests for validate-deps.py with mock installed_plugins.json.

Validates plugin dependency checking logic:
- Plugin presence detection
- Version comparison (including "unknown" handling)
- Stderr error message format (BLOCKED prefix, batch install command)
"""

import importlib.util
import json
import os
import sys
import tempfile
import unittest
from io import StringIO
from unittest.mock import patch

# Path to the actual script under test
_SCRIPT_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "validate-deps.py")


def _make_mock_plugins(entries):
    """Create a mock installed_plugins.json file and return its path.

    Args:
        entries: dict mapping "name@source" -> dict with version and optional fields.

    Returns:
        Absolute path to the temporary JSON file (caller must clean up).
    """
    data = {"version": 2, "plugins": {}}
    for key, entry in entries.items():
        data["plugins"][key] = [
            {
                "version": entry.get("version", "unknown"),
                "scope": entry.get("scope", "global"),
                "installPath": entry.get("installPath", "/tmp/mock"),
            }
        ]
    fd, path = tempfile.mkstemp(suffix=".json", prefix="mock_plugins_")
    os.close(fd)
    with open(path, "w") as f:
        json.dump(data, f)
    return path


def _get_check_plugins():
    """Load validate-deps.py and return the check_plugins function.

    Raises ImportError if the script file cannot be found or loaded.
    This is intentionally called inside each test so that a missing
    implementation produces individually failing tests (4 FAILED in
    pytest) rather than a single collection-level error.
    """
    if not os.path.exists(_SCRIPT_PATH):
        raise ImportError(f"validate-deps.py not found at {_SCRIPT_PATH}")
    spec = importlib.util.spec_from_file_location(
        "_internal_validate_deps", _SCRIPT_PATH
    )
    if spec is None or spec.loader is None:
        raise ImportError(f"Could not create module spec for {_SCRIPT_PATH}")
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except FileNotFoundError:
        raise ImportError(f"validate-deps.py not found at {_SCRIPT_PATH}")
    return module.check_plugins


class TestValidateDeps(unittest.TestCase):
    """Test suite for validate-deps.py check_plugins()."""

    def test_all_plugins_present(self):
        """Both required plugins present with valid versions -> True."""
        check_plugins = _get_check_plugins()
        path = _make_mock_plugins({
            "superpowers@claude-plugins-official": {"version": "5.1.0"},
            "pr-review-toolkit@claude-plugins-official": {"version": "unknown"},
        })
        try:
            result = check_plugins(path)
            self.assertTrue(result)
        finally:
            os.unlink(path)

    def test_unknown_version_satisfies_minimum(self):
        """Both plugins 'unknown' versions, superpowers min_version=5.1.0 -> True."""
        check_plugins = _get_check_plugins()
        path = _make_mock_plugins({
            "superpowers@claude-plugins-official": {"version": "unknown"},
            "pr-review-toolkit@claude-plugins-official": {"version": "unknown"},
        })
        try:
            result = check_plugins(path)
            self.assertTrue(result)
        finally:
            os.unlink(path)

    def test_superpowers_missing(self):
        """Missing superpowers -> False, BLOCKED message with install hint."""
        check_plugins = _get_check_plugins()
        path = _make_mock_plugins({
            "pr-review-toolkit@claude-plugins-official": {"version": "unknown"},
        })
        try:
            with patch("sys.stderr", new_callable=StringIO) as mock_stderr:
                result = check_plugins(path)
            self.assertFalse(result)
            stderr = mock_stderr.getvalue()
            self.assertIn("BLOCKED", stderr)
            self.assertIn("superpowers", stderr)
            self.assertIn("claude plugins install", stderr)
        finally:
            os.unlink(path)

    def test_superpowers_version_too_low(self):
        """superpowers 5.0.0 below minimum 5.1.0 -> False, version mismatch."""
        check_plugins = _get_check_plugins()
        path = _make_mock_plugins({
            "superpowers@claude-plugins-official": {"version": "5.0.0"},
            "pr-review-toolkit@claude-plugins-official": {"version": "unknown"},
        })
        try:
            with patch("sys.stderr", new_callable=StringIO) as mock_stderr:
                result = check_plugins(path)
            self.assertFalse(result)
            stderr = mock_stderr.getvalue()
            self.assertIn("BLOCKED", stderr)
            self.assertIn("5.0.0", stderr)
        finally:
            os.unlink(path)

    def test_pr_review_toolkit_missing(self):
        """Missing pr-review-toolkit -> False, BLOCKED message."""
        check_plugins = _get_check_plugins()
        path = _make_mock_plugins({
            "superpowers@claude-plugins-official": {"version": "5.1.0"},
        })
        try:
            with patch("sys.stderr", new_callable=StringIO) as mock_stderr:
                result = check_plugins(path)
            self.assertFalse(result)
            stderr = mock_stderr.getvalue()
            self.assertIn("BLOCKED", stderr)
            self.assertIn("pr-review-toolkit", stderr)
        finally:
            os.unlink(path)


if __name__ == "__main__":
    unittest.main()
