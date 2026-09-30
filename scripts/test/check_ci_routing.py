#!/usr/bin/env python3

import re
import sys
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
WORKSPACE_MANIFEST = WORKSPACE_ROOT / "Cargo.toml"
CI_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci.yml"
BENCHMARKS_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/benchmarks.yml"
DOCS_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/docs.yml"
REUSABLE_CHECK_MATRIX = (
    WORKSPACE_ROOT / ".github/workflows/reusable-check-matrix.yml"
)
PR_CLEANUP_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci-pr-cleanup.yml"
LEGACY_BRANCH_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci-branch-push.yml"
CI_PERF_PAGES_SCRIPT = WORKSPACE_ROOT / "scripts/test/ci_perf_pages.py"
MIRRORED_BENCHMARK_PAYLOADS: dict[Path, tuple[str, ...]] = {
    Path("qemu/compile-sim-bench"): (
        "compile-sim-bench.c",
        "compile-sim-bench-run.sh",
        "prebuild.sh",
        "linux-compile-sim-init.sh",
        "build-x86_64-unknown-none.toml",
    ),
    Path("qemu/ltp-hackbench"): (
        "ltp-hackbench.sh",
        "affinity_exec.c",
        "prebuild.sh",
        "build-x86_64-unknown-none.toml",
    ),
    Path("qemu/ltp-netstress"): (
        "ltp-netstress.sh",
        "prebuild.sh",
        "build-x86_64-unknown-none.toml",
    ),
}


def main() -> int:
    errors: list[str] = []
    errors.extend(check_mirrored_payload_consistency(WORKSPACE_ROOT))
    if not CI_WORKFLOW.is_file():
        errors.append("missing workflow: .github/workflows/ci.yml")
    if not REUSABLE_CHECK_MATRIX.is_file():
        errors.append(
            "missing workflow: .github/workflows/reusable-check-matrix.yml"
        )
    if not BENCHMARKS_WORKFLOW.is_file():
        errors.append("missing workflow: .github/workflows/benchmarks.yml")
    if not DOCS_WORKFLOW.is_file():
        errors.append("missing workflow: .github/workflows/docs.yml")
    if not CI_PERF_PAGES_SCRIPT.is_file():
        errors.append("missing script: scripts/test/ci_perf_pages.py")
    if PR_CLEANUP_WORKFLOW.exists():
        errors.append("stale-run cleanup must reuse the Plan CI runner")
    if LEGACY_BRANCH_WORKFLOW.exists():
        errors.append("branch push routing must be part of ci.yml")
    if errors:
        return report(errors)

    ci_workflow = CI_WORKFLOW.read_text(encoding="utf-8")
    benchmarks_workflow = BENCHMARKS_WORKFLOW.read_text(encoding="utf-8")
    docs_workflow = DOCS_WORKFLOW.read_text(encoding="utf-8")
    reusable_check_matrix = REUSABLE_CHECK_MATRIX.read_text(encoding="utf-8")
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
    for workflow_path in (
        ".github/workflows/starry-apps.yml",
        ".github/workflows/axvisor-nightly.yml",
        ".github/workflows/benchmarks.yml",
        ".github/workflows/docs.yml",
    ):
        if workflow_path not in pull_request_paths:
            errors.append(
                f"pull_request paths must include {workflow_path} so workflow-only "
                "changes run CI routing validation"
            )
    if "PR_HEAD_REPOSITORY_OWNER" in ci_workflow:
        errors.append("runner planning must not use the pull request source owner")

    matrix_step = named_step_block(plan_ci, "Plan check matrices")
    for fragment, message in (
        (
            "HEAD_REPOSITORY: ${{ github.event.pull_request.head.repo.full_name || '' }}",
            "runner planning must receive the pull request head repository",
        ),
        (
            '--head-repository "$HEAD_REPOSITORY"',
            "runner planning must distinguish fork pull requests",
        ),
        (
            "ACTOR: ${{ github.actor }}",
            "runner trust evidence must record the workflow actor",
        ),
        (
            "HEAD_SHA: ${{ github.event.pull_request.head.sha || github.sha }}",
            "runner trust evidence must record the tested head revision",
        ),
        (
            'if [ "$EVENT_NAME" = "pull_request" ]',
            "runner trust evidence must evaluate pull requests explicitly",
        ),
        (
            '[ "$HEAD_REPOSITORY" != "$GITHUB_REPOSITORY" ]',
            "runner trust evidence must reject cross-repository heads",
        ),
    ):
        require_contains(errors, matrix_step, fragment, message)

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
                "queued|pending|in_progress|waiting|requested)",
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
        "stale-run cleanup must only execute for same-repository pull requests",
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
                "PR cleanup must reject an empty head branch",
            ),
            (
                'env.PR_HEAD_REPOSITORY_ID != ""',
                "PR cleanup must reject an empty head repository ID",
            ),
            (
                ".head_branch == env.PR_HEAD_REF",
                "PR cleanup must match the head branch",
            ),
            (
                ".head_repository.id == (env.PR_HEAD_REPOSITORY_ID | tonumber)",
                "PR cleanup must match the stable head repository ID",
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
        # Starry and AxVisor combine independent QEMU and board targets. A failed board
        # must not cancel the other targets and discard their test evidence.
        fail_fast = "false" if output_prefix in ("starry", "axvisor") else "true"
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
            (f"fail_fast: {fail_fast}", "must preserve its failure collection policy"),
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
    reusable_jobs = mapping_block(reusable_check_matrix, "jobs", 0)
    reusable_run = mapping_block(reusable_jobs, "run", 2)
    for fragment, message in (
        (
            "github.event_name == 'pull_request'",
            "runner allocation must identify pull request events",
        ),
        (
            "github.event.pull_request.head.repo.full_name != github.repository",
            "runner allocation must identify cross-repository heads",
        ),
        (
            "'ubuntu-latest' || matrix.runs_on",
            "fork pull requests must allocate only a GitHub-hosted runner",
        ),
    ):
        require_contains(errors, reusable_run, fragment, message)

    if "perf-data" in benchmarks_workflow:
        errors.append("benchmark history updates must not use the legacy branch")
    if (
        "perf-history-axvisor" in benchmarks_workflow
        or "perf-history-starry" in benchmarks_workflow
    ):
        errors.append("benchmark history must use one Pages update bridge")
    for fragment, message in (
        ("git push", "benchmarks must not push a legacy history branch"),
        ("push --force", "benchmarks must not force-push a legacy history branch"),
        ("git commit-tree", "benchmarks must not build legacy history commits"),
        ("git mktree", "benchmarks must not build legacy history trees"),
    ):
        if fragment in benchmarks_workflow:
            errors.append(message)

    benchmark_jobs = mapping_block(benchmarks_workflow, "jobs", 0)
    benchmark_updates = mapping_block(benchmark_jobs, "benchmark-updates", 2)
    benchmark_updates_condition = mapping_block(
        benchmark_updates.replace("if: >-", "if:"),
        "if",
        4,
    )
    benchmark_permissions = mapping_block(benchmark_updates, "permissions", 4)
    axvisor_download = named_step_block(
        benchmark_updates,
        "Download AxVisor performance reports",
    )
    require_contains(
        errors,
        benchmark_permissions,
        "actions: write",
        "benchmark updates must dispatch the Pages workflow",
    )
    require_contains(
        errors,
        axvisor_download,
        "if: needs.axvisor_performance.result == 'success'",
        "AxVisor reports must only be downloaded from a successful matrix",
    )
    for matrix_name, description in (
        ("plan", "planning"),
        ("axvisor_performance", "AxVisor performance"),
        ("starry_performance", "Starry performance"),
        ("starry_board_performance", "Starry board performance"),
    ):
        require_contains(
            errors,
            benchmark_updates_condition,
            f"needs.{matrix_name}.result == 'success'",
            f"benchmark updates must require successful {description}",
        )
    for fragment, message in (
        (
            "needs.plan.result == 'success'",
            "benchmark updates must wait for the tested revision",
        ),
        (
            "continue-on-error: true",
            "performance report downloads must tolerate missing artifacts",
        ),
        (
            "needs.axvisor_performance.result == 'success'",
            "AxVisor updates must require the performance matrix",
        ),
        (
            "needs.starry_performance.result == 'success'",
            "Starry updates must require the QEMU matrix",
        ),
        (
            "needs.starry_board_performance.result == 'success'",
            "Starry updates must require the board matrix",
        ),
        (
            "steps.updates.outputs.has_updates == 'true'",
            "docs must not be dispatched without benchmark updates",
        ),
        (
            "name: benchmark-updates",
            "benchmark updates must be handed off as an artifact",
        ),
        (
            "path: ${{ runner.temp }}/benchmark-updates/*.json",
            "the bridge artifact must contain only this run's increments",
        ),
        (
            "retention-days: 30",
            "the benchmark bridge artifact must cover a docs recovery window",
        ),
        (
            "gh workflow run docs.yml --ref dev",
            "benchmark updates must dispatch the Pages workflow",
        ),
        (
            '-f benchmark_run_id="${BENCHMARK_RUN_ID}"',
            "docs must receive the benchmark run ID",
        ),
        (
            '-f benchmark_revision="${BENCHMARK_REVISION}"',
            "docs must receive the benchmark revision",
        ),
        (
            '-f benchmark_date="${BENCHMARK_DATE}"',
            "docs must receive the benchmark date",
        ),
        (
            "INCLUDE_AXVISOR: ${{ needs.axvisor_performance.result == 'success' }}",
            "AxVisor report collection must have an inclusion gate",
        ),
        (
            'if [ "${INCLUDE_AXVISOR}" = "true" ]; then',
            "AxVisor reports must only be collected from an included source",
        ),
    ):
        require_contains(errors, benchmark_updates, fragment, message)

    docs_permissions = mapping_block(docs_workflow, "permissions", 0)
    for fragment, message in (
        (
            "actions: read",
            "docs must read benchmark artifacts from the benchmark run",
        ),
        ("contents: read", "docs must keep read-only repository access"),
        ("pages: write", "docs must remain the Pages publisher"),
        ("id-token: write", "docs must keep the Pages deployment identity"),
    ):
        require_contains(errors, docs_permissions, fragment, message)

    docs_triggers = mapping_block(docs_workflow, "on", 0)
    docs_dispatch = mapping_block(docs_triggers, "workflow_dispatch", 2)
    for input_name in (
        "benchmark_run_id",
        "benchmark_revision",
        "benchmark_date",
    ):
        require_contains(
            errors,
            docs_dispatch,
            f"{input_name}:",
            f"docs dispatch must accept {input_name}",
        )

    docs_concurrency = mapping_block(docs_workflow, "concurrency", 0)
    require_contains(
        errors,
        docs_concurrency,
        "group: docs-pages",
        "docs deployments must share one Pages queue",
    )
    require_contains(
        errors,
        docs_concurrency,
        "queue: max",
        "docs deployments must preserve queued Pages work",
    )
    if "cancel-in-progress" in docs_concurrency:
        errors.append("docs deployments must not cancel queued Pages work")

    docs_jobs = mapping_block(docs_workflow, "jobs", 0)
    docs_build = mapping_block(docs_jobs, "build", 2)
    docs_setup = named_step_block(docs_build, "Set up Pages")
    require_contains(
        errors,
        docs_setup,
        "id: pages",
        "docs must expose the Pages base URL to the dashboard step",
    )
    docs_download = named_step_block(docs_build, "Download benchmark updates")
    for fragment, message in (
        (
            "github.event_name == 'workflow_dispatch'",
            "benchmark updates must only be downloaded for dispatched docs runs",
        ),
        (
            "inputs.benchmark_run_id != ''",
            "benchmark updates must identify the benchmark run",
        ),
        (
            "name: benchmark-updates",
            "docs must download the benchmark bridge artifact",
        ),
        (
            "run-id: ${{ inputs.benchmark_run_id }}",
            "docs must download the benchmark artifact from its source run",
        ),
        (
            "github-token: ${{ github.token }}",
            "cross-run artifact downloads need the workflow token",
        ),
    ):
        require_contains(errors, docs_download, fragment, message)

    docs_dashboard = named_step_block(docs_build, "Prepare performance dashboard")
    for fragment, message in (
        (
            "PAGES_BASE_URL: ${{ steps.pages.outputs.base_url }}",
            "docs must forward the deployed benchmark base URL to the script",
        ),
        (
            "BENCHMARK_UPDATES: ${{ runner.temp }}/benchmark-updates",
            "docs must forward the benchmark updates directory",
        ),
        (
            "python3 scripts/test/ci_perf_pages.py",
            "docs must delegate dashboard preparation to the script",
        ),
        (
            '--base-url "${PAGES_BASE_URL}"',
            "docs must pass the deployed benchmark base URL",
        ),
        (
            "--output-dir docs/build",
            "docs must pass the Pages output directory",
        ),
        (
            '--updates-dir "${BENCHMARK_UPDATES}"',
            "docs must pass the benchmark updates directory",
        ),
        (
            '--benchmark-run-id "${BENCHMARK_RUN_ID}"',
            "docs must pass the benchmark run ID",
        ),
        (
            '--benchmark-revision "${BENCHMARK_REVISION}"',
            "docs must pass the benchmark revision",
        ),
        (
            '--benchmark-date "${BENCHMARK_DATE}"',
            "docs must pass the benchmark date",
        ),
    ):
        require_contains(errors, docs_dashboard, fragment, message)
    for fragment, message in (
        ("curl ", "dashboard fetch logic must live in ci_perf_pages.py"),
        (
            "--header 'Cache-Control: no-cache'",
            "published benchmark reads must live in ci_perf_pages.py",
        ),
        (
            "cache_buster=${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}",
            "the cache buster must live in ci_perf_pages.py",
        ),
        (
            "git fetch --depth=1 origin perf-data",
            "legacy bootstrap must live in ci_perf_pages.py",
        ),
        (
            "git show FETCH_HEAD:history.json",
            "legacy bootstrap must live in ci_perf_pages.py",
        ),
        (
            "::error::Failed to fetch",
            "benchmark fetch failures must live in ci_perf_pages.py",
        ),
        (
            "::error::Benchmark updates require published or legacy dashboard data",
            "the history seed check must live in ci_perf_pages.py",
        ),
        (
            "::error::Unexpected published dashboard state",
            "the published state check must live in ci_perf_pages.py",
        ),
    ):
        if fragment in docs_dashboard:
            errors.append(message)
    if "perf-data" in docs_workflow:
        errors.append(
            "the frozen legacy branch bootstrap must live in ci_perf_pages.py"
        )
    for fragment, message in (
        ("git push", "docs must not push a legacy history branch"),
        ("push --force", "docs must not force-push a legacy history branch"),
        ("git commit-tree", "docs must not build legacy history commits"),
        ("git mktree", "docs must not build legacy history trees"),
    ):
        if fragment in docs_workflow:
            errors.append(message)
    docs_deploy = mapping_block(docs_jobs, "deploy", 2)
    require_contains(
        errors,
        docs_deploy,
        "uses: actions/deploy-pages@v5",
        "docs must keep the existing Pages deploy job",
    )

    return report(errors)


def workspace_source_roots() -> set[str]:
    manifest = WORKSPACE_MANIFEST.read_text(encoding="utf-8")
    members = manifest.split("members = [", maxsplit=1)[1].split("]", maxsplit=1)[0]
    package_paths = re.findall(r'^\s+"([^"]+)",?$', members, flags=re.MULTILINE)
    package_paths.extend(re.findall(r'\bpath\s*=\s*"([^"]+)"', manifest))
    return {Path(package_path).parts[0] for package_path in package_paths}


def check_mirrored_payload_consistency(workspace_root: Path) -> list[str]:
    errors: list[str] = []
    for case_dir, file_names in MIRRORED_BENCHMARK_PAYLOADS.items():
        smoke_dir = workspace_root / "apps/starry" / case_dir
        benchmark_dir = workspace_root / "benchmarks/starry" / case_dir
        for file_name in file_names:
            smoke_path = smoke_dir / file_name
            benchmark_path = benchmark_dir / file_name
            if not smoke_path.is_file():
                errors.append(
                    "missing mirrored benchmark payload file: "
                    f"{smoke_path.relative_to(workspace_root).as_posix()}"
                )
            if not benchmark_path.is_file():
                errors.append(
                    "missing mirrored benchmark payload file: "
                    f"{benchmark_path.relative_to(workspace_root).as_posix()}"
                )
            if not smoke_path.is_file() or not benchmark_path.is_file():
                continue
            if smoke_path.read_bytes() != benchmark_path.read_bytes():
                errors.append(
                    "mirrored benchmark payload files must remain byte-identical: "
                    f"{smoke_path.relative_to(workspace_root).as_posix()} and "
                    f"{benchmark_path.relative_to(workspace_root).as_posix()} differ"
                )
    return errors


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
