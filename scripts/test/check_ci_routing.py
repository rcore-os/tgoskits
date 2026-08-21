#!/usr/bin/env python3

import re
import sys
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
WORKSPACE_MANIFEST = WORKSPACE_ROOT / "Cargo.toml"
CI_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci.yml"
REUSABLE_CHECK_MATRIX = (
    WORKSPACE_ROOT / ".github/workflows/reusable-check-matrix.yml"
)
PR_CLEANUP_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci-pr-cleanup.yml"
LEGACY_BRANCH_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci-branch-push.yml"


def main() -> int:
    errors: list[str] = []
    if not CI_WORKFLOW.is_file():
        errors.append("missing workflow: .github/workflows/ci.yml")
    if not REUSABLE_CHECK_MATRIX.is_file():
        errors.append(
            "missing workflow: .github/workflows/reusable-check-matrix.yml"
        )
    if not PR_CLEANUP_WORKFLOW.is_file():
        errors.append("missing workflow: .github/workflows/ci-pr-cleanup.yml")
    if LEGACY_BRANCH_WORKFLOW.exists():
        errors.append("branch push routing must be part of ci.yml")
    if errors:
        return report(errors)

    ci_workflow = CI_WORKFLOW.read_text(encoding="utf-8")
    reusable_check_matrix = REUSABLE_CHECK_MATRIX.read_text(encoding="utf-8")
    pr_cleanup_workflow = PR_CLEANUP_WORKFLOW.read_text(encoding="utf-8")
    ci_triggers = mapping_block(ci_workflow, "on", 0)
    ci_push = mapping_block(ci_triggers, "push", 2)
    pull_request = mapping_block(ci_triggers, "pull_request", 2)
    jobs = mapping_block(ci_workflow, "jobs", 0)
    plan_ci = mapping_block(jobs, "plan_ci", 2)
    if mapping_block(ci_push, "branches", 4):
        errors.append("main CI push trigger must accept every branch")

    concurrency = mapping_block(ci_workflow, "concurrency", 0)
    for protected_ref in ("main", "dev"):
        require_contains(
            errors,
            concurrency,
            f"github.ref == 'refs/heads/{protected_ref}'",
            f"{protected_ref} CI runs must use a stable per-ref concurrency group",
        )
    for fragment, message in (
        (
            "github.event_name != 'pull_request'",
            "pull request runs must not enter main or dev concurrency queues",
        ),
        (
            "format('ci-{0}-{1}', github.workflow, github.ref)",
            "main and dev CI queues must be isolated by ref",
        ),
        (
            "format('ci-{0}-{1}', github.workflow, github.run_id)",
            "non-protected runs must use unique concurrency groups",
        ),
        ("queue: max", "main and dev CI queues must preserve every commit"),
    ):
        require_contains(errors, concurrency, fragment, message)

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

    route_step = named_step_block(plan_ci, "Route duplicate events")
    if not route_step:
        errors.append("Plan CI must have a duplicate-event routing step")
    else:
        pull_request_route = shell_if_block(
            route_step, '"$EVENT_NAME" = "pull_request"'
        )
        if not pull_request_route:
            errors.append("duplicate routing must identify pull request events")
        for fragment, message in (
            (
                '[ "$HEAD_REPOSITORY" = "$GITHUB_REPOSITORY" ]',
                "pull request routing must identify same-repository branches",
            ),
            (
                "actions/workflows/ci.yml/runs",
                "pull request routing must query runs from the same CI workflow",
            ),
            (
                "-f event=push",
                "pull request routing must only reuse push runs",
            ),
            (
                '-f branch="$PR_HEAD_REF"',
                "pull request routing must match the head branch",
            ),
            (
                '-f head_sha="$HEAD_SHA"',
                "pull request routing must match the head commit",
            ),
            (
                "for ((attempt = 1; attempt <= 10; attempt++))",
                "pull request routing must retry while its push run becomes visible",
            ),
            (
                "sleep 2",
                "push run discovery retries must remain bounded and observable",
            ),
            (
                'actions/runs/${run_id}/jobs',
                "completed push routing must inspect the push run matrix jobs",
            ),
            (
                "--paginate",
                "push matrix inspection must include every jobs page",
            ),
            (
                '.name != "Plan CI"',
                "a completed Plan-only push run must not suppress pull request CI",
            ),
            (
                '.conclusion != "skipped"',
                "a skipped push matrix must not suppress pull request CI",
            ),
            (
                '.conclusion != "cancelled"',
                "a cancelled push matrix must not suppress pull request CI",
            ),
            (
                '"repos/${GITHUB_REPOSITORY}/actions/runs/${run_id}"',
                "pull request routing must recheck the push run before skipping",
            ),
            (
                '[.status, (.conclusion != "cancelled")] | @tsv',
                "push run recheck must return its current lifecycle state",
            ),
            (
                "queued|in_progress|waiting|requested)",
                "only known unfinished push states may suppress pull request CI",
            ),
            (
                "completed)",
                "completed push runs must prove that matrix jobs exist",
            ),
            (
                "*)",
                "unknown push states must fail open",
            ),
            (
                "should_run=false",
                "a matching canonical push run must skip duplicate pull request CI",
            ),
        ):
            require_contains(errors, pull_request_route, fragment, message)
        if pull_request_route.count("should_run=false") != 1:
            errors.append(
                "only a pull request backed by a canonical push run may disable CI"
            )
        if pull_request_route.count("--paginate") != 2:
            errors.append("both push runs and push jobs queries must paginate")
        query_failure_check = pull_request_route.find(
            'if [ "$route_query_failed" = "true" ]'
        )
        reusable_run_check = pull_request_route.rfind(
            'if [ -n "$reusable_push_runs" ]'
        )
        if (
            query_failure_check == -1
            or reusable_run_check == -1
            or query_failure_check > reusable_run_check
        ):
            errors.append(
                "any push run query failure must fail open before reusing another run"
            )
        for fragment in ('"$EVENT_NAME" = "push"', "gh pr list"):
            if fragment in route_step:
                errors.append(
                    "push events must remain canonical and must not be disabled by PR state"
                )

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
            "needs.plan_ci.outputs.should_run == 'true'",
            "matrix jobs must follow the branch routing decision",
        ),
        (
            "github.ref == 'refs/heads/main' || github.ref == 'refs/heads/dev'",
            "only main and dev pushes may save caches",
        ),
    ):
        require_contains(errors, ci_workflow, fragment, message)

    cancel_step = named_step_block(plan_ci, "Cancel older queued or running runs")
    if "steps.route.outputs.should_run" in cancel_step:
        errors.append(
            "stale-run cleanup must run even when duplicate pull request CI is skipped"
        )
    for protected_ref in ("main", "dev"):
        require_contains(
            errors,
            cancel_step,
            f"github.ref != 'refs/heads/{protected_ref}'",
            f"stale-run cleanup must preserve every {protected_ref} push run",
        )
    normal_cancel = cancel_step.find(
        '"repos/${GITHUB_REPOSITORY}/actions/runs/${run_id}/cancel"'
    )
    force_cancel = cancel_step.find(
        '"repos/${GITHUB_REPOSITORY}/actions/runs/${run_id}/force-cancel"'
    )
    if normal_cancel == -1 or force_cancel == -1 or normal_cancel > force_cancel:
        errors.append(
            "stale-run cleanup must force-cancel runs that ignore normal cancellation"
        )
    require_contains(
        errors,
        cancel_step,
        "github.event.pull_request.head.repo.full_name == github.repository",
        "fork PR cleanup must use the trusted pull_request_target workflow",
    )
    require_contains(
        errors,
        cancel_step,
        "github.event_name != 'pull_request'",
        "the non-protected ref branch must not re-enable fork PR cleanup",
    )

    pull_request_selector, push_selector = shell_if_else_branches(
        ci_workflow,
        '"$EVENT_NAME" = "pull_request"',
    )
    if not pull_request_selector or not push_selector:
        errors.append("missing event-specific stale-run selectors")
    else:
        for fragment, message in (
            (
                "PR_HEAD_REF: ${{ github.event.pull_request.head.ref || '' }}",
                "PR cleanup must receive the pull request head branch",
            ),
            (
                "PR_HEAD_REPOSITORY_ID: ${{ github.event.pull_request.head.repo.id || '' }}",
                "PR cleanup must receive the pull request head repository ID",
            ),
        ):
            require_contains(errors, cancel_step, fragment, message)
        for fragment, message in (
            (
                '.event == "pull_request"',
                "PR cleanup must only select pull request runs",
            ),
            (
                ".pull_requests[]?",
                "PR cleanup must select runs for the same pull request",
            ),
            (
                "(.pull_requests | length) == 0",
                "PR cleanup must detect runs without pull request links",
            ),
            (
                'env.PR_HEAD_REF != ""',
                "fork PR cleanup must reject an empty head branch",
            ),
            (
                'env.PR_HEAD_REPOSITORY_ID != ""',
                "fork PR cleanup must reject an empty head repository ID",
            ),
            (
                ".head_branch == env.PR_HEAD_REF",
                "fork PR cleanup must match the head branch",
            ),
            (
                ".head_repository.id == (env.PR_HEAD_REPOSITORY_ID | tonumber)",
                "fork PR cleanup must match the stable head repository ID",
            ),
        ):
            require_contains(errors, pull_request_selector, fragment, message)
        if '.event == "push"' in pull_request_selector:
            errors.append("PR cleanup must not cancel matching push runs")
        for fragment, message in (
            (
                ".event == env.EVENT_NAME",
                "non-PR cleanup must only cancel runs from the same event",
            ),
        ):
            require_contains(errors, push_selector, fragment, message)

    if mapping_block(jobs, "test_checks", 2):
        errors.append("the legacy Verification caller must be removed")
    if re.search(r"^\s+name:\s+Verification\s*$", ci_workflow, re.MULTILINE):
        errors.append("the CI job list must not expose a Verification group")
    if "test_matrix" in ci_workflow:
        errors.append("the workflow must consume per-group matrices, not test_matrix")

    grouped_jobs = (
        ("workspace_checks", "Workspace", "workspace"),
        ("arceos_checks", "ArceOS", "arceos"),
        ("starry_checks", "Starry", "starry"),
        ("axvisor_checks", "AxVisor", "axvisor"),
    )
    for job_id, display_name, output_prefix in grouped_jobs:
        job = mapping_block(jobs, job_id, 2)
        if not job:
            errors.append(f"missing grouped CI job: {job_id}")
            continue
        for fragment, message in (
            (f"name: {display_name}", "must expose the expected group name"),
            ("- plan_ci", "must depend on Plan CI"),
            ("- static_checks", "must depend on Preflight"),
            ("always()", "must evaluate after Preflight finishes"),
            (
                "needs.plan_ci.result == 'success'",
                "must require successful planning",
            ),
            (
                "needs.plan_ci.outputs.should_run == 'true'",
                "must follow branch routing",
            ),
            (
                f"needs.plan_ci.outputs.{output_prefix}_required == 'true'",
                "must follow its planner selection",
            ),
            (
                "needs.static_checks.result == 'success'",
                "must require a successful Preflight",
            ),
            (
                "needs.static_checks.result == 'skipped'",
                "must accept an intentionally skipped Preflight",
            ),
            (
                "uses: ./.github/workflows/reusable-check-matrix.yml",
                "must call the reusable matrix executor",
            ),
            (
                f"needs.plan_ci.outputs.{output_prefix}_matrix",
                "must consume its planner matrix",
            ),
            ("fail_fast: true", "must keep fail-fast within the group"),
            ("save_cache: >-", "must preserve cache-save routing"),
            (
                "since_ref: ${{ needs.plan_ci.outputs.since_ref }}",
                "must receive the incremental base",
            ),
            (
                "CLAW_API_KEY: ${{ secrets.CLAW_API_KEY }}",
                "must preserve optional credentials",
            ),
        ):
            require_contains(
                errors,
                job,
                fragment,
                f"{display_name} {message}",
            )
        for output_name in ("matrix", "required"):
            require_contains(
                errors,
                ci_workflow,
                f"steps.matrix.outputs.{output_prefix}_{output_name}",
                f"Plan CI must publish {output_prefix}_{output_name}",
            )

    max_parallel_input = mapping_block(
        reusable_check_matrix,
        "max_parallel",
        6,
    )
    for fragment, message in (
        ("type: number", "matrix parallelism must use a numeric limit"),
        ("default: 256", "matrix entries must not be serialized by default"),
    ):
        require_contains(errors, max_parallel_input, fragment, message)
    strategy = mapping_block(reusable_check_matrix, "strategy", 4)
    require_contains(
        errors,
        strategy,
        "max-parallel: ${{ inputs.max_parallel }}",
        "the reusable matrix must honor its parallelism limit",
    )

    for fragment, message in (
        (
            "pull_request_target:",
            "fork PR cleanup must run in a trusted target context",
        ),
        ("actions: write", "fork PR cleanup must receive an Actions write token"),
        (
            '"repos/${GITHUB_REPOSITORY}/pulls/${PR_NUMBER}"',
            "fork PR cleanup must re-read the current PR head",
        ),
        (
            'if [ "$PR_EVENT_HEAD_SHA" != "$PR_HEAD_SHA" ]',
            "a delayed cleanup event must not cancel a newer PR head",
        ),
        (
            '.head_sha != env.PR_HEAD_SHA',
            "fork PR cleanup must preserve every run for the current head",
        ),
        (
            ".pull_requests[]?",
            "fork PR cleanup must prefer the pull request number",
        ),
        (
            ".head_repository.id == (env.PR_HEAD_REPOSITORY_ID | tonumber)",
            "fork PR fallback must match the stable source repository ID",
        ),
        (
            'actions/runs/${run_id}/cancel',
            "fork PR cleanup must request normal cancellation",
        ),
        (
            'actions/runs/${run_id}/force-cancel',
            "fork PR cleanup must force-cancel a run that remains active",
        ),
    ):
        require_contains(errors, pr_cleanup_workflow, fragment, message)
    if "actions/checkout" in pr_cleanup_workflow:
        errors.append("pull_request_target cleanup must never checkout PR code")

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


def named_step_block(text: str, name: str) -> str:
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if line.strip() != f"- name: {name}":
            continue
        indent = len(line) - len(line.lstrip())
        block = [line]
        for nested_line in lines[index + 1 :]:
            nested_indent = len(nested_line) - len(nested_line.lstrip())
            if nested_line and nested_indent <= indent:
                break
            block.append(nested_line)
        return "\n".join(block)
    return ""


def shell_if_else_branches(text: str, condition: str) -> tuple[str, str]:
    match = re.search(
        rf'^\s*if \[ {re.escape(condition)} \]; then\s*$\n'
        r"(?P<then>.*?)"
        r"^\s*else\s*$\n"
        r"(?P<else>.*?)"
        r"^\s*fi\s*$",
        text,
        flags=re.MULTILINE | re.DOTALL,
    )
    if match is None:
        return "", ""
    return match.group("then"), match.group("else")


def shell_if_block(text: str, condition: str) -> str:
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if not line.strip().startswith("if ") or condition not in line:
            continue
        depth = 0
        block = []
        for nested_line in lines[index:]:
            stripped = nested_line.strip()
            if stripped.startswith("if "):
                depth += 1
            block.append(nested_line)
            if stripped == "fi":
                depth -= 1
                if depth == 0:
                    return "\n".join(block)
    return ""


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
