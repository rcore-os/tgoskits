#!/usr/bin/env python3
"""PreToolUse hook: block PR creation and direct push unless clean branch + local CI pass.
Reads the command from CLAUDE_TOOL_INPUT env var. Exits 0 silently for non-gate commands."""
import os
import subprocess
import sys

def get_tool_input():
    """Read tool input from env var, fall back to stdin JSON."""
    env_val = os.environ.get("CLAUDE_TOOL_INPUT", "")
    if env_val:
        return env_val
    # Fallback: read JSON from stdin (alternative hook protocol)
    try:
        import json as _json
        data = _json.load(sys.stdin)
        return data.get("input", data.get("command", ""))
    except Exception:
        return ""

tool_input = get_tool_input()

# Only gate gh pr create and git push commands
if "gh pr create" not in tool_input and "git push" not in tool_input:
    sys.exit(0)

workspace = os.environ.get("CLAUDE_WORKDIR", os.getcwd())
cache_dir = os.path.join(workspace, ".claude", "cache")


def check_clean_base():
    """Verify current branch is based on upstream/dev HEAD."""
    try:
        subprocess.run(
            ["git", "fetch", "upstream", "dev"],
            capture_output=True, cwd=workspace, timeout=30
        )
    except Exception:
        pass

    result = subprocess.run(
        ["git", "rev-parse", "upstream/dev"],
        capture_output=True, text=True, cwd=workspace
    )
    if result.returncode != 0:
        # Try origin/dev
        try:
            subprocess.run(
                ["git", "fetch", "origin", "dev"],
                capture_output=True, cwd=workspace, timeout=30
            )
        except Exception:
            pass
        result = subprocess.run(
            ["git", "rev-parse", "origin/dev"],
            capture_output=True, text=True, cwd=workspace
        )
    upstream_head = result.stdout.strip()

    merge_base = subprocess.run(
        ["git", "merge-base", "HEAD", upstream_head],
        capture_output=True, text=True, cwd=workspace
    ).stdout.strip()

    if merge_base != upstream_head:
        print(
            "BLOCKED: Branch is not based on upstream/dev HEAD.\n"
            "Create a clean branch first:\n"
            "  git fetch upstream dev\n"
            "  git checkout -b <feature-branch> upstream/dev",
            file=sys.stderr
        )
        return False
    return True


def check_local_ci():
    """Check if local CI has passed."""
    ci_file = os.path.join(cache_dir, "last-ci-result.json")
    if not os.path.exists(ci_file):
        print(
            "BLOCKED: Local CI has not been run.\n"
            "Run at minimum:\n"
            "  bash .claude/scripts/local-ci.sh quick",
            file=sys.stderr
        )
        return False

    import json
    try:
        with open(ci_file) as f:
            data = json.load(f)
        if data.get("status") != "pass":
            print(
                "BLOCKED: Local CI did not pass.\n"
                f"Status: {data.get('status', 'unknown')}\n"
                "Fix the issues and re-run CI.",
                file=sys.stderr
            )
            return False
    except (json.JSONDecodeError, KeyError):
        print(
            "BLOCKED: Local CI result file is corrupted.\n"
            "Re-run: bash .claude/scripts/local-ci.sh quick",
            file=sys.stderr
        )
        return False
    return True


def check_direct_push():
    """Block direct pushes to main/dev."""
    if "git push" in tool_input:
        # Check if pushing to main or dev
        current_branch = subprocess.run(
            ["git", "branch", "--show-current"],
            capture_output=True, text=True, cwd=workspace
        ).stdout.strip()

        if current_branch in ("main", "dev"):
            print(
                "BLOCKED: Direct push to main/dev is forbidden.\n"
                "Use a feature branch and create a PR.",
                file=sys.stderr
            )
            return False
    return True


# Run checks
for check in (check_clean_base, check_local_ci, check_direct_push):
    if not check():
        sys.exit(1)

sys.exit(0)
