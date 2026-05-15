#!/usr/bin/env python3
"""SessionStart hook: validate required plugins are installed with minimum versions.

Reads ~/.claude/plugins/installed_plugins.json. Exits 0 when all deps satisfied;
exits 1 with clear error and batch fix command when deps are missing.

This module is both importable (exposes check_plugins()) and executable.
"""

import json
import os
import sys

REQUIRED_PLUGINS = {
    "superpowers@claude-plugins-official": {
        "min_version": "5.1.0",
        "purpose": "systematic-debugging, verification-before-completion, brainstorming skills",
    },
    "pr-review-toolkit@claude-plugins-official": {
        "min_version": None,  # any version is accepted
        "purpose": "code-reviewer, silent-failure-hunter, pr-test-analyzer agents",
    },
}


def parse_version(ver_str):
    """Parse a version string like '5.1.0' into a comparable tuple.

    Special values:
      - "unknown" returns (0,) which satisfies any minimum version check.
      - Any parse failure (ValueError, AttributeError) returns (0,).

    Returns:
        tuple of ints suitable for comparison.
    """
    if ver_str == "unknown":
        return (sys.maxsize,)
    try:
        return tuple(int(x) for x in ver_str.split("."))
    except (ValueError, AttributeError):
        return (0,)


def check_plugins(plugins_path=None):
    """Check that all required plugins are installed with valid versions.

    Args:
        plugins_path: Path to installed_plugins.json (optional). When None,
                      defaults to ~/.claude/plugins/installed_plugins.json.

    Returns:
        True if all requirements are satisfied, False otherwise.
        Error messages are printed to stderr when requirements are not met.
    """
    if plugins_path is None:
        plugins_path = os.path.expanduser("~/.claude/plugins/installed_plugins.json")

    resolved_path = os.path.realpath(plugins_path)

    if not os.path.exists(resolved_path):
        print(
            "BLOCKED: Required plugins are not installed.\n"
            "Install them with a single command:\n"
            "  claude plugins install superpowers pr-review-toolkit",
            file=sys.stderr,
        )
        return False

    try:
        with open(resolved_path, "r") as f:
            data = json.load(f)
    except json.JSONDecodeError:
        print(
            "BLOCKED: Corrupted installed_plugins.json.\n"
            "Reinstall the plugins:\n"
            "  claude plugins install superpowers pr-review-toolkit",
            file=sys.stderr,
        )
        return False

    installed = data.get("plugins", {})
    missing = []

    for plugin_key, req in REQUIRED_PLUGINS.items():
        entries = installed.get(plugin_key, [])
        if not entries or not isinstance(entries, list):
            missing.append((plugin_key, "not installed", req["purpose"]))
            continue

        installed_ver = entries[0].get("version", "unknown")
        min_ver = req["min_version"]

        if min_ver is not None and parse_version(installed_ver) < parse_version(min_ver):
            missing.append(
                (
                    plugin_key,
                    f"version {installed_ver} (need >= {min_ver})",
                    req["purpose"],
                )
            )

    if missing:
        print("BLOCKED: Required plugins missing or outdated:\n", file=sys.stderr)
        for name, detail, purpose in missing:
            plugin_display = name.split("@")[0]
            print(f"  - {plugin_display}: {detail}", file=sys.stderr)
            print(f"    Provides: {purpose}", file=sys.stderr)
        print("\nFix with a single command:", file=sys.stderr)
        plugin_names = " ".join(m[0].split("@")[0] for m in missing)
        print(f"  claude plugins install {plugin_names}", file=sys.stderr)
        return False

    return True


if __name__ == "__main__":
    sys.exit(0 if check_plugins() else 1)
