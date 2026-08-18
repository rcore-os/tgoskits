#!/usr/bin/env python3

import sys
from pathlib import Path


WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
CI_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci.yml"
BRANCH_PUSH_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci-branch-push.yml"
CONTAINER_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/container-publish.yml"
STATIC_CHECKS = WORKSPACE_ROOT / ".github/ci/checks/static.toml"
WORKSPACE_CHECKS = WORKSPACE_ROOT / ".github/ci/checks/workspace.toml"
MAIN_BRANCHES = {"main", "dev"}
CI_CHECK_PATHS = {
    ".cargo/**",
    ".github/actions/publish-container/action.yml",
    ".github/ci/**",
    ".github/workflows/ci-branch-push.yml",
    ".github/workflows/container-publish.yml",
    ".github/workflows/ci.yml",
    ".github/workflows/reusable-check-matrix.yml",
    ".github/workflows/starry-apps.yml",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "apps/**",
    "bootloader/**",
    "components/**",
    "drivers/**",
    "fs/**",
    "memory/**",
    "net/**",
    "os/**",
    "platforms/**",
    "scripts/**",
    "test-suit/**",
    "tools/**",
    "virtualization/**",
    "xtask/**",
    "!**/*.md",
}


def main() -> int:
    errors: list[str] = []

    required_files = (CI_WORKFLOW, BRANCH_PUSH_WORKFLOW, CONTAINER_WORKFLOW)
    for required_file in required_files:
        if not required_file.is_file():
            errors.append(f"missing workflow: {required_file.relative_to(WORKSPACE_ROOT)}")
    if errors:
        return report(errors)

    ci_workflow = CI_WORKFLOW.read_text(encoding="utf-8")
    branch_push_workflow = BRANCH_PUSH_WORKFLOW.read_text(encoding="utf-8")
    container_workflow = CONTAINER_WORKFLOW.read_text(encoding="utf-8")
    static_checks = STATIC_CHECKS.read_text(encoding="utf-8")
    workspace_checks = WORKSPACE_CHECKS.read_text(encoding="utf-8")

    ci_triggers = mapping_block(ci_workflow, "on", 0)
    ci_push = mapping_block(ci_triggers, "push", 2)
    ci_pull_request = mapping_block(ci_triggers, "pull_request", 2)
    ci_dispatch = mapping_block(ci_triggers, "workflow_dispatch", 2)
    ci_dispatch_inputs = mapping_block(ci_dispatch, "inputs", 4)
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
    require_equal(
        errors,
        "main CI pull_request paths",
        list_items(ci_pull_request, "paths", 4),
        CI_CHECK_PATHS,
    )
    pull_request_paths = list_items_in_order(ci_pull_request, "paths", 4)
    if not pull_request_paths or pull_request_paths[-1] != "!**/*.md":
        errors.append("the Markdown exclusion must be the final pull_request path rule")
    if scalar_value(since_sha, "type", 8) != "string":
        errors.append("since_sha must be a workflow_dispatch string input")
    for removed_input in ("run_target", "container_target"):
        if mapping_block(ci_dispatch_inputs, removed_input, 6):
            errors.append(f"main CI must not retain the {removed_input} input")

    require_contains(
        errors,
        ci_workflow,
        "since_ref: ${{ steps.since.outputs.since_ref }}",
        "plan_ci must publish the resolved since_ref",
    )
    require_contains(
        errors,
        ci_workflow,
        "github.event.pull_request.head.repo.owner.login || ''",
        "PR planning must select checks using the source repository owner",
    )
    require_contains(
        errors,
        ci_workflow,
        'source_repository_owner="$PR_HEAD_REPOSITORY_OWNER"',
        "PR planning must not fall back to the base owner when head metadata is absent",
    )
    require_contains(
        errors,
        ci_workflow,
        '--repository-owner "$source_repository_owner"',
        "the planner must receive the resolved source repository owner",
    )
    require_contains(
        errors,
        ci_workflow,
        '--since-ref "$SINCE_REF"',
        "the planner must receive the resolved PR base revision",
    )
    require_contains(
        errors,
        ci_workflow,
        '--summary-file "$GITHUB_STEP_SUMMARY"',
        "the planner must publish its PR impact summary",
    )
    require_contains(
        errors,
        static_checks,
        'cargo xtask sync-lint --since "$SINCE_REF"',
        "sync-lint must consume SINCE_REF from the reusable runner",
    )
    require_contains(
        errors,
        workspace_checks,
        'cargo xtask clippy --since "$SINCE_REF"',
        "clippy must consume SINCE_REF from the reusable runner",
    )
    require_contains(
        errors,
        workspace_checks,
        'cargo xtask test --since "$SINCE_REF"',
        "incremental std tests must consume SINCE_REF from the reusable runner",
    )
    for event_branch in (
        'if [ "$EVENT_NAME" = "pull_request" ]; then',
        'elif [ "$EVENT_NAME" = "push" ]',
        'elif [ "$EVENT_NAME" = "workflow_dispatch" ]',
    ):
        require_contains(
            errors,
            ci_workflow,
            event_branch,
            f"since_ref resolution is missing {event_branch}",
        )
    if "publish_base_container" in ci_workflow or "container_target" in ci_workflow:
        errors.append("container publishing must not remain in the main CI workflow")

    container_triggers = mapping_block(container_workflow, "on", 0)
    container_push = mapping_block(container_triggers, "push", 2)
    container_dispatch = mapping_block(container_triggers, "workflow_dispatch", 2)
    container_inputs = mapping_block(container_dispatch, "inputs", 4)
    target = mapping_block(container_inputs, "target", 6)
    require_equal(
        errors,
        "container publish branches",
        list_items(container_push, "branches", 4),
        MAIN_BRANCHES,
    )
    require_equal(
        errors,
        "container publish paths",
        list_items(container_push, "paths", 4),
        {
            "container/Dockerfile",
            "container/Dockerfile.axvisor-lvz",
            "rust-toolchain.toml",
        },
    )
    require_equal(
        errors,
        "container target options",
        list_items(target, "options", 8),
        {"base", "axvisor-lvz", "both"},
    )
    if scalar_value(target, "default", 8) != "both":
        errors.append("container target must default to both")

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

    for needle, message in (
        ("gh pr list", "router must detect an existing open pull request"),
        ("--state open", "router lookup must ignore closed pull requests"),
        (
            "headRepositoryOwner.login == env.REPOSITORY_OWNER",
            "router must match the pull request head repository owner",
        ),
        (
            "headRefName == env.REF_NAME",
            "router must match the pull request head branch",
        ),
        ("gh workflow run ci.yml", "router must dispatch main CI"),
        ('-f since_sha="$BEFORE_SHA"', "router must preserve the push base SHA"),
    ):
        require_contains(errors, branch_push_workflow, needle, message)
    if "run_target" in branch_push_workflow or "container_target" in branch_push_workflow:
        errors.append("branch router must not pass removed CI dispatch inputs")

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
    return set(list_items_in_order(text, key, indent))


def list_items_in_order(text: str, key: str, indent: int) -> list[str]:
    block = mapping_block(text, key, indent)
    return [
        line.strip()[2:].strip().strip('"')
        for line in block.splitlines()
        if line.strip().startswith("- ")
    ]


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
