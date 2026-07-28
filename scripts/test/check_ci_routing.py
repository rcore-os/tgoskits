#!/usr/bin/env python3

import sys
from pathlib import Path


WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
CI_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci.yml"
BRANCH_PUSH_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci-branch-push.yml"
MAIN_BRANCHES = {"main", "dev"}


def main() -> int:
    errors: list[str] = []

    if not BRANCH_PUSH_WORKFLOW.is_file():
        errors.append(
            "feature branch pushes must be handled by .github/workflows/ci-branch-push.yml"
        )
        return report(errors)

    ci_workflow = CI_WORKFLOW.read_text(encoding="utf-8")
    branch_push_workflow = BRANCH_PUSH_WORKFLOW.read_text(encoding="utf-8")

    ci_triggers = mapping_block(ci_workflow, "on", 0)
    ci_push = mapping_block(ci_triggers, "push", 2)
    ci_pull_request = mapping_block(ci_triggers, "pull_request", 2)
    ci_dispatch = mapping_block(ci_triggers, "workflow_dispatch", 2)
    ci_dispatch_inputs = mapping_block(ci_dispatch, "inputs", 4)
    run_target = mapping_block(ci_dispatch_inputs, "run_target", 6)
    since_sha = mapping_block(ci_dispatch_inputs, "since_sha", 6)

    router_push = mapping_block(
        mapping_block(branch_push_workflow, "on", 0), "push", 2
    )
    router_permissions = mapping_block(branch_push_workflow, "permissions", 0)

    require_equal(
        errors,
        "main CI push branches",
        list_items(ci_push, "branches", 4),
        MAIN_BRANCHES,
    )
    require_equal(
        errors,
        "branch push router ignored branches",
        list_items(router_push, "branches-ignore", 4),
        MAIN_BRANCHES,
    )
    require_equal(
        errors,
        "push paths-ignore rules",
        list_items(ci_push, "paths-ignore", 4),
        list_items(router_push, "paths-ignore", 4),
    )
    if not ci_pull_request:
        errors.append("main CI must retain the pull_request trigger")

    require_equal(
        errors,
        "run_target options",
        list_items(run_target, "options", 8),
        {"container", "ci"},
    )
    if scalar_value(run_target, "default", 8) != "container":
        errors.append("run_target must default to container publishing")
    if scalar_value(since_sha, "type", 8) != "string":
        errors.append("since_sha must be a workflow_dispatch string input")

    require_contains(
        errors,
        ci_workflow,
        "since_ref: ${{ steps.outputs.outputs.since_ref }}",
        "detect_changes must publish the resolved since_ref",
    )
    require_contains(
        errors,
        ci_workflow,
        'cargo xtask sync-lint --since "${{ needs.detect_changes.outputs.since_ref }}"',
        "sync-lint must consume the resolved since_ref",
    )
    require_contains(
        errors,
        ci_workflow,
        'cargo xtask clippy --since "${{ needs.detect_changes.outputs.since_ref }}"',
        "clippy must consume the resolved since_ref",
    )
    require_contains(
        errors,
        ci_workflow,
        'if [ "$EVENT_NAME" = "pull_request" ]; then',
        "since_ref resolution must handle pull_request events",
    )
    require_contains(
        errors,
        ci_workflow,
        '[ "$EVENT_NAME" = "workflow_dispatch" ] && [ "$RUN_TARGET" = "ci" ]',
        "since_ref resolution must handle routed workflow_dispatch events",
    )
    require_contains(
        errors,
        ci_workflow,
        'printf \'%s\\n\' "$SINCE_SHA"',
        "routed CI must use the push base SHA supplied by the router",
    )
    if "skip_duplicate_push_ci" in ci_workflow:
        errors.append("duplicate-push detection must not remain in the main CI workflow")

    expected_permissions = {
        "actions": "write",
        "contents": "read",
        "pull-requests": "read",
    }
    actual_permissions = {
        name: scalar_value(router_permissions, name, 2)
        for name in expected_permissions
    }
    if actual_permissions != expected_permissions:
        errors.append(
            "branch push router permissions mismatch: "
            f"expected {expected_permissions}, got {actual_permissions}"
        )

    require_contains(
        errors,
        branch_push_workflow,
        "gh pr list",
        "router must detect an existing open pull request",
    )
    require_contains(
        errors,
        branch_push_workflow,
        "--state open",
        "router pull request lookup must ignore closed pull requests",
    )
    require_contains(
        errors,
        branch_push_workflow,
        "headRepositoryOwner.login == env.REPOSITORY_OWNER",
        "router must match the pull request head repository owner",
    )
    require_contains(
        errors,
        branch_push_workflow,
        "headRefName == env.REF_NAME",
        "router must match the pull request head branch",
    )
    require_contains(
        errors,
        branch_push_workflow,
        "gh workflow run ci.yml",
        "router must dispatch the main CI when no pull request exists",
    )
    require_contains(
        errors,
        branch_push_workflow,
        "-f run_target=ci",
        "router dispatch must select CI checks",
    )
    require_contains(
        errors,
        branch_push_workflow,
        '-f since_sha="$BEFORE_SHA"',
        "router dispatch must preserve the push base SHA",
    )

    skip_index = branch_push_workflow.find("exit 0")
    dispatch_index = branch_push_workflow.find("gh workflow run ci.yml")
    if skip_index == -1 or dispatch_index == -1 or skip_index > dispatch_index:
        errors.append(
            "open-PR routing must exit successfully before the full-CI dispatch path"
        )

    return report(errors)


def mapping_block(text: str, key: str, indent: int) -> str:
    marker = f"{' ' * indent}{key}:"
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if line != marker:
            continue
        block: list[str] = []
        for nested_line in lines[index + 1 :]:
            if nested_line and indentation(nested_line) <= indent:
                break
            block.append(nested_line)
        return "\n".join(block)
    return ""


def list_items(text: str, key: str, indent: int) -> set[str]:
    block = mapping_block(text, key, indent)
    return {
        line.strip()[2:].strip().strip('"')
        for line in block.splitlines()
        if line.strip().startswith("- ")
    }


def scalar_value(text: str, key: str, indent: int) -> str:
    marker = f"{' ' * indent}{key}:"
    for line in text.splitlines():
        if line.startswith(marker):
            return line.removeprefix(marker).strip().strip('"')
    return ""


def indentation(line: str) -> int:
    return len(line) - len(line.lstrip())


def require_contains(
    errors: list[str], text: str, needle: str, message: str
) -> None:
    if needle not in text:
        errors.append(message)


def require_equal(
    errors: list[str], label: str, actual: set[str], expected: set[str]
) -> None:
    if actual != expected:
        errors.append(
            f"{label} mismatch: expected {sorted(expected)}, got {sorted(actual)}"
        )


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
