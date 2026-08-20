#!/usr/bin/env python3

import re
import sys
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
WORKSPACE_MANIFEST = WORKSPACE_ROOT / "Cargo.toml"
CI_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci.yml"
LEGACY_BRANCH_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci-branch-push.yml"


def main() -> int:
    errors: list[str] = []
    if not CI_WORKFLOW.is_file():
        errors.append("missing workflow: .github/workflows/ci.yml")
    if LEGACY_BRANCH_WORKFLOW.exists():
        errors.append("branch push routing must be part of ci.yml")
    if errors:
        return report(errors)

    ci_workflow = CI_WORKFLOW.read_text(encoding="utf-8")
    ci_triggers = mapping_block(ci_workflow, "on", 0)
    ci_push = mapping_block(ci_triggers, "push", 2)
    pull_request = mapping_block(ci_triggers, "pull_request", 2)
    if mapping_block(ci_push, "branches", 4):
        errors.append("main CI push trigger must accept every branch")

    pull_request_paths = list_items_in_order(pull_request, "paths", 4)
    if not pull_request_paths or pull_request_paths[-1] != "!**/*.md":
        errors.append("the Markdown exclusion must be the final pull_request path rule")
    missing_roots = sorted(
        f"{root}/**"
        for root in workspace_source_roots()
        if f"{root}/**" not in pull_request_paths
    )
    if missing_roots:
        errors.append(
            "pull_request paths omit workspace roots: " + ", ".join(missing_roots)
        )
    if "PR_HEAD_REPOSITORY_OWNER" in ci_workflow:
        errors.append("runner planning must not use the pull request source owner")

    for fragment, message in (
        (
            '--repository-owner "$REPOSITORY_OWNER"',
            "runner planning must use the workflow repository owner",
        ),
        (
            '--since-ref "$SINCE_REF"',
            "the planner must receive the incremental base revision",
        ),
        (
            '--summary-file "$GITHUB_STEP_SUMMARY"',
            "the planner must publish its impact summary",
        ),
        (
            "needs.plan_ci.outputs.static_required == 'true'",
            "Preflight must follow the planner decision",
        ),
        (
            "needs.static_checks.result == 'skipped'",
            "Verification must accept an intentionally skipped Preflight",
        ),
        ("gh pr list", "the plan job must detect open pull requests"),
        ("should_run=false", "an open PR must disable duplicate branch push CI"),
        (
            "needs.plan_ci.outputs.should_run == 'true'",
            "matrix jobs must follow the branch routing decision",
        ),
        (
            "github.ref == 'refs/heads/main' || github.ref == 'refs/heads/dev'",
            "only main and dev pushes may save caches",
        ),
    ):
        require_contains(errors, ci_workflow, fragment, message)

    return report(errors)


def workspace_source_roots() -> set[str]:
    manifest = WORKSPACE_MANIFEST.read_text(encoding="utf-8")
    members = manifest.split("members = [", maxsplit=1)[1].split("]", maxsplit=1)[0]
    package_paths = re.findall(r'^\s+"([^"]+)",?$', members, flags=re.MULTILINE)
    package_paths.extend(re.findall(r'\bpath\s*=\s*"([^"]+)"', manifest))
    return {Path(package_path).parts[0] for package_path in package_paths}


def mapping_block(text: str, key: str, indent: int) -> str:
    marker = f"{' ' * indent}{key}:"
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if line != marker:
            continue
        block = []
        for nested_line in lines[index + 1 :]:
            if nested_line and len(nested_line) - len(nested_line.lstrip()) <= indent:
                break
            block.append(nested_line)
        return "\n".join(block)
    return ""


def list_items_in_order(text: str, key: str, indent: int) -> list[str]:
    return [
        line.strip()[2:].strip().strip('"')
        for line in mapping_block(text, key, indent).splitlines()
        if line.strip().startswith("- ")
    ]


def require_contains(errors: list[str], text: str, fragment: str, message: str) -> None:
    if fragment not in text:
        errors.append(message)


def report(errors: list[str]) -> int:
    if not errors:
        print("CI routing checks passed")
        return 0
    print("CI routing checks failed:", file=sys.stderr)
    for error in errors:
        print(f"  - {error}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
