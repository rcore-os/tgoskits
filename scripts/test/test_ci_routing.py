#!/usr/bin/env python3

import json
import os
import re
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path
from types import SimpleNamespace

from scripts.test.check_ci_routing import (
    check_mirrored_payload_consistency,
    list_items_in_order,
    mapping_block,
    named_step_block,
)
WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
CI_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci.yml"
STARRY_APPS_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/starry-apps.yml"
AXVISOR_NIGHTLY_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/axvisor-nightly.yml"
BENCHMARKS_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/benchmarks.yml"
DOCS_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/docs.yml"
REUSABLE_CHECK_MATRIX = (
    WORKSPACE_ROOT / ".github/workflows/reusable-check-matrix.yml"
)
PR_CLEANUP_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci-pr-cleanup.yml"
AXVISOR_NIGHTLY_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/axvisor-nightly.yml"


class MirroredPayloadTests(unittest.TestCase):
    def test_current_shared_payloads_are_consistent(self) -> None:
        self.assertEqual(check_mirrored_payload_consistency(WORKSPACE_ROOT), [])

    def test_shared_payload_discovery_does_not_need_case_names(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            smoke = root / "apps/starry/generated-case"
            benchmark = root / "benchmarks/starry/generated-case"
            smoke.mkdir(parents=True)
            benchmark.mkdir(parents=True)
            (smoke / "payload.sh").write_text("same\n", encoding="utf-8")
            (benchmark / "payload.sh").write_text("same\n", encoding="utf-8")
            self.assertEqual(check_mirrored_payload_consistency(root), [])
            (benchmark / "payload.sh").write_text("changed\n", encoding="utf-8")
            errors = check_mirrored_payload_consistency(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("generated-case/payload.sh", errors[0])

            (smoke / "payload.sh").unlink()
            errors = check_mirrored_payload_consistency(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("missing mirrored benchmark payload file", errors[0])

    def test_shared_build_configuration_must_remain_consistent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            smoke = root / "apps/starry/generated-case"
            benchmark = root / "benchmarks/starry/generated-case"
            smoke.mkdir(parents=True)
            benchmark.mkdir(parents=True)
            (smoke / "build-x86_64-unknown-none.toml").write_text(
                "features = []\n", encoding="utf-8"
            )
            (benchmark / "build-x86_64-unknown-none.toml").write_text(
                "features = []\n", encoding="utf-8"
            )
            (smoke / "qemu-x86_64.toml").write_text("{}\n", encoding="utf-8")
            (benchmark / "qemu-x86_64-benchmark.toml").write_text(
                "{}\n", encoding="utf-8"
            )

            self.assertEqual(check_mirrored_payload_consistency(root), [])
            (benchmark / "build-x86_64-unknown-none.toml").write_text(
                "features = ['changed']\n", encoding="utf-8"
            )
            errors = check_mirrored_payload_consistency(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("build-x86_64-unknown-none.toml", errors[0])
            (benchmark / "build-x86_64-unknown-none.toml").unlink()
            errors = check_mirrored_payload_consistency(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("missing mirrored benchmark payload file", errors[0])


class ReleasePrerequisiteTests(unittest.TestCase):
    def test_semver_checks_install_libudev_before_release_plz(self) -> None:
        workflow = (
            WORKSPACE_ROOT / ".github/workflows/release-plz.yml"
        ).read_text(encoding="utf-8")
        job = mapping_block(workflow, "release-plz-pr", 2)
        setup = named_step_block(job, "Install semver-check system dependencies")

        self.assertTrue(setup, "semver checks require the libudev development files")
        self.assertIn("sudo apt-get update", setup)
        self.assertRegex(
            setup,
            r"sudo apt-get install --yes (?:pkg-config libudev-dev|libudev-dev pkg-config)",
        )
        self.assertLess(job.index(setup), job.index("- name: Run release-plz"))


class RunnerTrustTests(unittest.TestCase):
    def test_public_job_images_do_not_require_registry_login(self) -> None:
        workflow = REUSABLE_CHECK_MATRIX.read_text(encoding="utf-8")
        job = mapping_block(workflow, "run", 2)
        container = mapping_block(job, "container", 4)
        credentials = mapping_block(container, "credentials", 6)

        # A nonempty username prevents the runner's implicit GITHUB_TOKEN
        # fallback; an omitted password makes ContainerRegistryLogin skip login.
        self.assertIn("        username: anonymous", credentials.splitlines())
        # Actions rejects an explicitly empty password before any job starts.
        self.assertNotRegex(credentials, r"(?m)^\s*password:")

    def test_cleanup_reuses_planning_runner(self) -> None:
        self.assertFalse(
            PR_CLEANUP_WORKFLOW.exists(),
            "stale-run cleanup must not allocate a separate workflow runner",
        )
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        job = mapping_block(workflow, "plan_ci", 2)
        cleanup = named_step_block(job, "Cancel older queued or running runs")
        self.assertTrue(cleanup)
        self.assertIn("actions: write", mapping_block(job, "permissions", 4))
        self.assertLess(
            job.index("- name: Cancel older queued or running runs"),
            job.index("- name: Checkout code"),
        )
        self.assertNotIn("steps.route.outputs.should_run", cleanup)

    def test_cross_repository_pr_can_enter_hosted_planning(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        job = mapping_block(workflow, "plan_ci", 2)
        condition = mapping_block(job.replace("if: >-", "if:"), "if", 4)
        self.assertIn("github.event_name == 'pull_request'", condition)

    def test_fork_pr_matrix_is_forced_onto_github_hosted_runner(self) -> None:
        workflow = REUSABLE_CHECK_MATRIX.read_text(encoding="utf-8")
        job = mapping_block(workflow, "run", 2)

        self.assertIn(
            "github.event_name == 'pull_request'",
            job,
        )
        self.assertIn(
            "github.event.pull_request.head.repo.full_name != github.repository",
            job,
        )
        self.assertIn("'ubuntu-latest' || matrix.runs_on", job)

    def test_fork_push_keeps_its_own_workflow_entry(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        triggers = mapping_block(workflow, "on", 0)
        push = mapping_block(triggers, "push", 2)
        self.assertTrue(push)
        self.assertFalse(mapping_block(push, "branches", 4))
        job = mapping_block(workflow, "plan_ci", 2)
        condition = mapping_block(job.replace("if: >-", "if:"), "if", 4)
        self.assertIn("github.event_name == 'push'", condition)
        self.assertNotIn("rcore-os", condition)


class PlannerContractTests(unittest.TestCase):
    def test_plan_inputs_are_bound_to_action_forwarding(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        plan_job = mapping_block(workflow, "plan_ci", 2)
        matrix_step = named_step_block(plan_job, "Plan check matrices")
        action = (WORKSPACE_ROOT / ".github/actions/ci-plan/action.yml").read_text(
            encoding="utf-8"
        )

        for fragment in (
            "repository-owner: ${{ github.repository_owner }}",
            "head-repository: ${{ github.event.pull_request.head.repo.full_name || '' }}",
            "base-ref: ${{ github.base_ref }}",
            "since-ref: ${{ steps.since.outputs.since_ref }}",
        ):
            with self.subTest(fragment=fragment):
                self.assertIn(fragment, matrix_step)
        for fragment in (
            '--repository-owner "$REPOSITORY_OWNER"',
            'args+=(--head-repository "$HEAD_REPOSITORY")',
            'args+=(--base-ref "$BASE_REF")',
            'args+=(--since-ref "$SINCE_REF")',
            'args+=(--summary-file "$GITHUB_STEP_SUMMARY")',
        ):
            with self.subTest(fragment=fragment):
                self.assertIn(fragment, action)

    def test_ci_test_discovery_rejects_a_zero_test_run(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        step = named_step_block(
            mapping_block(workflow, "plan_ci", 2), "Validate CI configuration"
        )
        self.assertIn('test_log="$RUNNER_TEMP/ci-tests.log"', step)
        self.assertRegex(step, r"grep -Eq '\^Ran \[1-9\]\[0-9\]\* tests\? in '")
        self.assertRegex(
            step,
            r"(?s)grep -Eq '\^Ran \[1-9\]\[0-9\]\* tests\? in '.*?\|\| \{.*?exit 1",
        )


class ConcurrencyRoutingTests(unittest.TestCase):
    def test_main_and_dev_use_distinct_fifo_groups(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        concurrency = mapping_block(workflow, "concurrency", 0)

        self.assertIn("github.event_name != 'pull_request'", concurrency)
        self.assertIn("github.ref == 'refs/heads/main'", concurrency)
        self.assertIn("github.ref == 'refs/heads/dev'", concurrency)
        self.assertIn(
            "format('ci-{0}-{1}', github.workflow, github.ref)", concurrency
        )
        self.assertIn(
            "format('ci-{0}-{1}', github.workflow, github.run_id)", concurrency
        )
        self.assertIn("queue: max", concurrency)

    def test_board_jobs_only_share_groups_on_protected_or_manual_runs(self) -> None:
        workflow = REUSABLE_CHECK_MATRIX.read_text(encoding="utf-8")
        job = mapping_block(mapping_block(workflow, "jobs", 0), "run", 2)
        concurrency = mapping_block(job, "concurrency", 4)
        match = re.search(
            r"group:\s*(?:>-\s*)?ci-resource-\${{(.*?)}}", concurrency, re.S
        )
        self.assertIsNotNone(match)
        expression = " ".join(match.group(1).split())
        expression = expression.replace("&&", " and ").replace("||", " or ")

        def group(
            event: str,
            ref: str,
            run_id: int,
            resource: str = "board",
            check_id: str = "check",
        ) -> str:
            # GitHub's &&/|| and format() use the same truthy short-circuit
            # behavior as Python's and/or and str.format for this expression.
            context = {
                "github": SimpleNamespace(event_name=event, ref=ref, run_id=run_id),
                "matrix": SimpleNamespace(id=check_id, resource_group=resource),
                "format": lambda template, *args: template.format(*args),
            }
            return "ci-resource-" + eval(expression, {"__builtins__": {}}, context)

        for event, ref in (
            ("push", "refs/heads/dev"),
            ("push", "refs/heads/main"),
            ("schedule", "refs/heads/dev"),
            ("workflow_dispatch", "refs/heads/topic"),
        ):
            with self.subTest(event=event, ref=ref):
                self.assertEqual(group(event, ref, 1), "ci-resource-board")
                self.assertEqual(group(event, ref, 2), "ci-resource-board")

        for event, ref in (
            ("push", "refs/heads/topic"),
            ("pull_request", "refs/pull/123/merge"),
        ):
            with self.subTest(event=event, ref=ref):
                self.assertEqual(group(event, ref, 1), "ci-resource-run-1-check")
                self.assertEqual(group(event, ref, 2), "ci-resource-run-2-check")
                self.assertEqual(
                    group(event, ref, 1, check_id="other"), "ci-resource-run-1-other"
                )

        self.assertEqual(
            group("push", "refs/heads/dev", 1, ""), "ci-resource-run-1-check"
        )
        self.assertIn("queue: max", concurrency)
        self.assertIn("cancel-in-progress: false", concurrency)


class AxvisorNightlyWorkflowTests(unittest.TestCase):
    def test_manual_dispatch_uses_the_selected_revision(self) -> None:
        workflow = AXVISOR_NIGHTLY_WORKFLOW.read_text(encoding="utf-8")
        plan = mapping_block(workflow, "plan", 2)
        concurrency = mapping_block(workflow, "concurrency", 0)

        self.assertNotIn("ref: dev", plan)
        self.assertIn("- name: Pin triggering revision", plan)
        self.assertIn('echo "sha=$(git rev-parse HEAD)"', plan)
        self.assertIn("axvisor-nightly-${{ github.ref }}", concurrency)
        self.assertIn("revision-label: tested revision", workflow)
        # Performance history moved to the benchmarks workflow, which pins the
        # dev revision; a nightly dispatch from any branch must stay a pure
        # check run and never publish history.
        self.assertNotIn("perf-history", workflow)


class NightlyResultPropagationTests(unittest.TestCase):
    def _result_scripts(self) -> dict[str, str]:
        # Verify each caller's with: mapping, then execute the actual shell body
        # from the composite action so its inputs and failure propagation stay
        # coupled to the implementation under test.
        action = (
            WORKSPACE_ROOT / ".github/actions/ci-result/action.yml"
        ).read_text(encoding="utf-8")
        action_step = named_step_block(action, "Render result")
        run_lines = action_step.splitlines()
        run_index = next(
            index for index, line in enumerate(run_lines) if line.strip() == "run: |"
        )
        action_script = textwrap.dedent("\n".join(run_lines[run_index + 1 :]))
        scripts = {}
        expected = (
            (
                "starry-apps",
                STARRY_APPS_WORKFLOW,
                ("Plan=${{ needs.plan.result }}", "Apps=${{ needs.checks.result }}"),
                ("Plan", "Apps"),
            ),
            (
                "axvisor-nightly",
                AXVISOR_NIGHTLY_WORKFLOW,
                ("Plan=${{ needs.plan.result }}", "Checks=${{ needs.checks.result }}"),
                ("Plan", "Checks"),
            ),
        )
        for label, workflow_path, stages, required in expected:
            workflow = workflow_path.read_text(encoding="utf-8")
            result_step = named_step_block(workflow, "Report result")
            self.assertIn("uses: ./.github/actions/ci-result", result_step)
            inputs = mapping_block(result_step, "with", 8)
            for stage in stages:
                self.assertIn(stage, inputs)
            for required_stage in required:
                self.assertIn(f"            {required_stage}", inputs)
            scripts[label] = (
                f"export STAGES=\"$(printf 'Plan=%s\\n{required[1]}=%s' "
                '"$PLAN_RESULT" "$CHECKS_RESULT")"\n'
                f"export REQUIRED_STAGES=\"$(printf 'Plan\\n{required[1]}')\"\n"
                + action_script
            )
        return scripts

    def test_result_jobs_propagate_a_failed_plan(self) -> None:
        for label, script in self._result_scripts().items():
            with self.subTest(workflow=label):
                completed = run_result_summary_step(
                    script, PLAN_RESULT="failure"
                )
                self.assertNotEqual(completed.returncode, 0)

    def test_result_jobs_propagate_failed_checks(self) -> None:
        for label, script in self._result_scripts().items():
            with self.subTest(workflow=label):
                completed = run_result_summary_step(
                    script, CHECKS_RESULT="failure"
                )
                self.assertNotEqual(completed.returncode, 0)

    def test_result_jobs_succeed_when_every_stage_passes(self) -> None:
        for label, script in self._result_scripts().items():
            with self.subTest(workflow=label):
                completed = run_result_summary_step(script)
                self.assertEqual(completed.returncode, 0)


class MatrixParallelismTests(unittest.TestCase):
    def test_self_hosted_matrix_waits_for_preflight_then_runs_in_parallel(
        self,
    ) -> None:
        ci_workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        jobs = mapping_block(ci_workflow, "jobs", 0)

        for job_name in (
            "workspace_checks",
            "arceos_checks",
            "starry_checks",
            "axvisor_checks",
        ):
            with self.subTest(job_name=job_name):
                job = mapping_block(jobs, job_name, 2)
                needs = mapping_block(job, "needs", 4)
                self.assertIn("- plan_ci", needs)
                self.assertIn("- static_checks", needs)

        reusable_workflow = REUSABLE_CHECK_MATRIX.read_text(encoding="utf-8")
        strategy = mapping_block(reusable_workflow, "strategy", 4)
        self.assertIn("max-parallel: ${{ inputs.max_parallel }}", strategy)
        self.assertRegex(
            reusable_workflow,
            r"(?ms)^      max_parallel:\n.*?^        default: (?:[2-9]|[1-9][0-9]+)$",
        )


class ScheduledWorkflowOwnershipTests(unittest.TestCase):
    def test_ci_pull_request_paths_cover_daily_workflows(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        triggers = mapping_block(workflow, "on", 0)
        pull_request = mapping_block(triggers, "pull_request", 2)
        paths = list_items_in_order(pull_request, "paths", 4)

        for workflow_path in (
            ".github/workflows/starry-apps.yml",
            ".github/workflows/axvisor-nightly.yml",
            ".github/workflows/benchmarks.yml",
            ".github/workflows/docs.yml",
        ):
            self.assertIn(workflow_path, paths)

    def test_benchmarks_workflow_owns_every_performance_matrix(self) -> None:
        workflow = BENCHMARKS_WORKFLOW.read_text(encoding="utf-8")
        triggers = mapping_block(workflow, "on", 0)
        schedule = mapping_block(triggers, "schedule", 2)
        jobs = mapping_block(workflow, "jobs", 0)
        plan = mapping_block(jobs, "plan", 2)
        plan_step = named_step_block(plan, "Plan benchmark matrices")

        self.assertIn('cron: "40 21 * * *"', schedule)
        self.assertIn("workflow_dispatch:", triggers)
        self.assertIn("mode: benchmarks", plan_step)
        # The owner of the performance history always measures the dev branch,
        # so a manual dispatch elsewhere cannot publish non-dev history.
        self.assertIn("ref: dev", plan)
        matrix_names = sorted(set(re.findall(r"steps\.matrix\.outputs\.([a-z0-9_]+)", plan)))
        self.assertTrue(matrix_names)
        for matrix_name in matrix_names:
            with self.subTest(matrix_name=matrix_name):
                job_id = next(
                    job_id
                    for job_id in re.findall(r"^  ([a-z0-9_-]+):$", jobs, re.M)
                    if f"matrix_json: ${{{{ needs.plan.outputs.{matrix_name} }}}}" in mapping_block(jobs, job_id, 2)
                )
                job = mapping_block(jobs, job_id, 2)
                self.assertIn("uses: ./.github/workflows/reusable-check-matrix.yml", job)

        board_jobs = [
            mapping_block(jobs, job_id, 2)
            for job_id in re.findall(r"^  ([a-z0-9_-]+):$", jobs, re.M)
            if "board" in job_id
        ]
        self.assertTrue(any("max_parallel: 1" in job for job in board_jobs))

        self.assertNotIn("perf-data", workflow)
        self.assertNotIn("perf-history-axvisor", workflow)
        self.assertNotIn("perf-history-starry", workflow)
        self.assertNotIn("git push", workflow)
        self.assertNotIn("push --force", workflow)
        self.assertNotIn("git commit-tree", workflow)
        self.assertNotIn("git mktree", workflow)

        benchmark_updates = mapping_block(jobs, "benchmark-updates", 2)
        self.assertTrue(benchmark_updates)
        benchmark_updates_condition = mapping_block(
            benchmark_updates.replace("if: >-", "if:"),
            "if",
            4,
        )
        self.assertIn("axvisor-nightly-performance-*", benchmark_updates)
        self.assertIn("continue-on-error: true", benchmark_updates)
        self.assertIn("starry-apps-nightly-performance-*", benchmark_updates)
        benchmark_job_ids = re.findall(r"^  ([a-z0-9_-]+):$", jobs, re.M)
        performance_jobs = {
            job_id
            for job_id in benchmark_job_ids
            if re.search(
                r"matrix_json: \$\{\{ needs\.plan\.outputs\.[a-z0-9_-]*performance_matrix \}\}",
                mapping_block(jobs, job_id, 2),
            )
        }
        expected_jobs = {"plan", *performance_jobs}
        self.assertTrue(performance_jobs)
        update_needs = set(list_items_in_order(benchmark_updates, "needs", 4))
        self.assertTrue(expected_jobs <= update_needs)
        for job_id in sorted(expected_jobs):
            with self.subTest(job_id=job_id):
                self.assertIn(f"needs.{job_id}.result == 'success'", benchmark_updates_condition)
        self.assertIn(
            "INCLUDE_AXVISOR: ${{ needs.axvisor_performance.result == 'success' }}",
            benchmark_updates,
        )
        axvisor_download = named_step_block(
            benchmark_updates,
            "Download AxVisor performance reports",
        )
        self.assertIn(
            "if: needs.axvisor_performance.result == 'success'",
            axvisor_download,
        )
        self.assertIn(
            'if [ "${INCLUDE_AXVISOR}" = "true" ]; then',
            benchmark_updates,
        )
        self.assertIn("name: benchmark-updates", benchmark_updates)
        self.assertIn("retention-days: 30", benchmark_updates)
        self.assertIn(
            "steps.updates.outputs.has_updates == 'true'",
            benchmark_updates,
        )
        self.assertIn("gh workflow run docs.yml --ref dev", benchmark_updates)
        for dispatch_input in (
            "benchmark_run_id",
            "benchmark_revision",
            "benchmark_date",
        ):
            self.assertIn(f"-f {dispatch_input}=", benchmark_updates)

    def test_docs_workflow_is_the_only_published_benchmark_writer(self) -> None:
        workflow = DOCS_WORKFLOW.read_text(encoding="utf-8")
        self.assertNotIn("git push", workflow)
        self.assertNotIn("push --force", workflow)

        triggers = mapping_block(workflow, "on", 0)
        dispatch = mapping_block(triggers, "workflow_dispatch", 2)
        for input_name in (
            "benchmark_run_id",
            "benchmark_revision",
            "benchmark_date",
        ):
            self.assertIn(f"{input_name}:", dispatch)

        permissions = mapping_block(workflow, "permissions", 0)
        self.assertIn("actions: read", permissions)
        self.assertIn("contents: read", permissions)
        self.assertIn("pages: write", permissions)
        self.assertIn("id-token: write", permissions)

        concurrency = mapping_block(workflow, "concurrency", 0)
        self.assertIn("group: docs-pages", concurrency)
        self.assertIn("queue: max", concurrency)
        self.assertNotIn("cancel-in-progress", concurrency)

        jobs = mapping_block(workflow, "jobs", 0)
        build = mapping_block(jobs, "build", 2)
        pages = named_step_block(build, "Set up Pages")
        self.assertIn("id: pages", pages)
        self.assertIn("uses: actions/configure-pages@v6", pages)

        download = named_step_block(build, "Download benchmark updates")
        self.assertIn(
            "github.event_name == 'workflow_dispatch'",
            download,
        )
        self.assertIn("inputs.benchmark_run_id != ''", download)
        self.assertIn("name: benchmark-updates", download)
        self.assertIn("run-id: ${{ inputs.benchmark_run_id }}", download)
        self.assertIn("github-token: ${{ github.token }}", download)

        prepare = named_step_block(build, "Prepare performance dashboard")
        for fragment in (
            "PAGES_BASE_URL: ${{ steps.pages.outputs.base_url }}",
            "BENCHMARK_UPDATES: ${{ runner.temp }}/benchmark-updates",
            "python3 scripts/test/ci_perf_pages.py",
            '--base-url "${PAGES_BASE_URL}"',
            "--output-dir docs/build",
            '--updates-dir "${BENCHMARK_UPDATES}"',
            '--benchmark-run-id "${BENCHMARK_RUN_ID}"',
            '--benchmark-revision "${BENCHMARK_REVISION}"',
            '--benchmark-date "${BENCHMARK_DATE}"',
        ):
            with self.subTest(fragment=fragment):
                self.assertIn(fragment, prepare)
        # The fetch, cache-buster, legacy bootstrap and merge branches moved
        # into ci_perf_pages.py, so the workflow step must not keep them.
        for fragment in (
            "curl ",
            "--header 'Cache-Control: no-cache'",
            "cache_buster=${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}",
            "git fetch --depth=1 origin perf-data",
            "git show FETCH_HEAD:history.json",
            "git show FETCH_HEAD:index.html",
            "::error::Failed to fetch",
            "::error::Unexpected published dashboard state",
            "docs/build/benchmark/index.html",
            "docs/build/benchmark/history.json",
        ):
            with self.subTest(removed_fragment=fragment):
                self.assertNotIn(fragment, prepare)
        self.assertNotIn("perf-data", workflow)
        self.assertLess(
            build.index("- name: Set up Pages"),
            build.index("- name: Prepare performance dashboard"),
        )
        self.assertLess(
            build.index("- name: Prepare performance dashboard"),
            build.index("- name: Upload Pages artifact"),
        )

        deploy = mapping_block(jobs, "deploy", 2)
        self.assertIn("name: github-pages", deploy)
        self.assertIn("uses: actions/deploy-pages@v5", deploy)

    def test_daily_workflows_do_not_own_benchmark_execution(self) -> None:
        starry_apps = STARRY_APPS_WORKFLOW.read_text(encoding="utf-8")
        axvisor_nightly = AXVISOR_NIGHTLY_WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("mode: starry-apps", starry_apps)
        self.assertIn("mode: axvisor-nightly", axvisor_nightly)
        for workflow in (starry_apps, axvisor_nightly):
            self.assertNotIn("benchmarks.toml", workflow)
            self.assertNotIn("performance_matrix", workflow)
            self.assertNotIn("axvisor-nightly-performance", workflow)
            self.assertNotIn("starry-apps-nightly-performance", workflow)
            self.assertNotIn("--source axvisor", workflow)
            self.assertNotIn("--source starry", workflow)
            self.assertNotIn("perf-data-publish", workflow)


class WifiSecretRoutingTests(unittest.TestCase):
    def test_non_wifi_matrix_rows_remove_empty_wifi_environment(self) -> None:
        workflow = REUSABLE_CHECK_MATRIX.read_text(encoding="utf-8")
        step = named_step_block(workflow, "Run command")

        self.assertIn("WIFI_SECRETS: ${{ matrix.wifi_secrets }}", step)
        self.assertIn('if [ "${WIFI_SECRETS}" != "true" ]; then', step)
        self.assertIn("unset STARRY_WIFI_SSID STARRY_WIFI_PASSWORD", step)


class ForkCleanupPermissionTests(unittest.TestCase):
    def test_main_cleanup_skips_fork_pull_requests(self) -> None:
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        cleanup_step = named_step_block(
            workflow,
            "Cancel older queued or running runs",
        )

        self.assertIn(
            "github.event.pull_request.head.repo.full_name == github.repository",
            cleanup_step,
        )
        self.assertIn("github.event_name != 'pull_request'", cleanup_step)


class DuplicateEventRoutingTests(unittest.TestCase):
    def test_push_event_always_runs_without_querying_pull_request_state(self) -> None:
        result = run_route(event_name="push")

        self.assertEqual(result.should_run, "true")
        self.assertEqual(result.gh_calls, [])

    def test_pull_request_skips_when_push_has_active_matrix(self) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs="101\t11975\thttps://example.test/push/101",
            has_active_matrix="true",
        )

        self.assertEqual(result.should_run, "false")
        self.assertIn("Duplicate pull request CI skipped", result.summary)
        self.assertTrue(
            any("actions/workflows/ci.yml/runs" in call for call in result.gh_calls)
        )
        self.assertTrue(
            any("actions/runs/101/jobs" in call for call in result.gh_calls)
        )

    def test_in_progress_push_suppresses_pull_request_before_matrix_exists(
        self,
    ) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs="101\t11975\thttps://example.test/push/101",
            push_run_status="in_progress",
        )

        self.assertEqual(result.should_run, "false")
        self.assertFalse(
            any("actions/runs/101/jobs" in call for call in result.gh_calls)
        )

    def test_pending_waiting_and_requested_pushes_suppress_pull_request(self) -> None:
        for status in ("pending", "waiting", "requested"):
            with self.subTest(status=status):
                result = run_route(
                    event_name="pull_request",
                    push_runs="101\t11975\thttps://example.test/push/101",
                    push_run_status=status,
                )

                self.assertEqual(result.should_run, "false")

    def test_unknown_push_status_does_not_suppress_pull_request(self) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs="101\t11975\thttps://example.test/push/101",
            push_run_status="mystery",
        )

        self.assertEqual(result.should_run, "true")

    def test_pull_request_retries_until_push_run_is_visible(self) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs="101\t11975\thttps://example.test/push/101",
            push_runs_after_query=2,
            push_run_status="queued",
        )

        self.assertEqual(result.should_run, "false")
        run_queries = [
            call
            for call in result.gh_calls
            if "actions/workflows/ci.yml/runs" in call
        ]
        self.assertGreaterEqual(len(run_queries), 2)

    def test_pull_request_runs_after_retry_when_no_push_appears(self) -> None:
        result = run_route(event_name="pull_request")

        self.assertEqual(result.should_run, "true")
        run_queries = [
            call
            for call in result.gh_calls
            if "actions/workflows/ci.yml/runs" in call
        ]
        self.assertEqual(len(run_queries), 10)

    def test_plan_only_push_does_not_suppress_pull_request(self) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs="101\t11975\thttps://example.test/push/101",
            has_active_matrix="false",
        )

        self.assertEqual(result.should_run, "true")

    def test_later_active_push_suppresses_pull_request(self) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs=(
                "101\t11975\thttps://example.test/push/101\n"
                "202\t11976\thttps://example.test/push/202"
            ),
            active_matrix_run_ids="202",
        )

        self.assertEqual(result.should_run, "false")
        self.assertTrue(any("actions/runs/101/jobs" in call for call in result.gh_calls))
        self.assertTrue(any("actions/runs/202/jobs" in call for call in result.gh_calls))

    def test_push_query_failure_runs_pull_request(self) -> None:
        result = run_route(event_name="pull_request", run_query_exit="1")

        self.assertEqual(result.should_run, "true")
        self.assertIn("Failed to query matching branch push runs", result.stdout)

    def test_push_job_query_failure_runs_pull_request(self) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs="101\t11975\thttps://example.test/push/101",
            job_query_exit="1",
        )

        self.assertEqual(result.should_run, "true")
        self.assertIn("Failed to inspect branch push run", result.stdout)

    def test_push_cancelled_before_route_decision_does_not_suppress_pull_request(
        self,
    ) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs="101\t11975\thttps://example.test/push/101",
            has_active_matrix="true",
            push_run_is_usable="false",
        )

        self.assertEqual(result.should_run, "true")
        self.assertTrue(
            any(
                "actions/runs/101" in call and "/jobs" not in call
                for call in result.gh_calls
            )
        )

    def test_push_recheck_failure_runs_pull_request(self) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs="101\t11975\thttps://example.test/push/101",
            has_active_matrix="true",
            push_run_query_exit="1",
        )

        self.assertEqual(result.should_run, "true")
        self.assertIn("Failed to recheck branch push run", result.stdout)

    def test_any_push_recheck_failure_overrides_another_reusable_run(self) -> None:
        result = run_route(
            event_name="pull_request",
            push_runs=(
                "101\t11975\thttps://example.test/push/101\n"
                "202\t11976\thttps://example.test/push/202"
            ),
            push_run_query_fail_ids="202",
            push_run_status="in_progress",
        )

        self.assertEqual(result.should_run, "true")
        self.assertIn("Failed to recheck branch push run #11976", result.stdout)

    def test_fork_pull_request_runs_without_querying_base_pushes(self) -> None:
        result = run_route(
            event_name="pull_request",
            head_repository="contributor/tgoskits",
        )

        self.assertEqual(result.should_run, "true")
        self.assertEqual(result.gh_calls, [])


class StaleRunCancellationTests(unittest.TestCase):
    def test_stuck_stale_run_is_force_cancelled(self) -> None:
        result = run_cancellation()

        self.assertTrue(
            any("actions/runs/101/cancel" in call for call in result.gh_calls)
        )
        self.assertTrue(
            any("actions/runs/101/force-cancel" in call for call in result.gh_calls)
        )

    def test_pull_request_cancels_only_older_matching_head_runs(self) -> None:
        result = run_cancellation(
            event_name="pull_request",
            pr_number="2078",
            pr_head_ref="feat/axvisor-ai-rtos-integration",
            pr_head_repository_id="1329374417",
            runs=[
                fake_run(
                    run_id=101,
                    run_number=100,
                    event="pull_request",
                    head_branch="feat/axvisor-ai-rtos-integration",
                    head_repository_id=1329374417,
                ),
                fake_run(
                    run_id=102,
                    run_number=101,
                    event="pull_request",
                    head_branch="another-branch",
                    head_repository_id=1329374417,
                ),
                fake_run(
                    run_id=103,
                    run_number=102,
                    event="pull_request",
                    head_branch="feat/axvisor-ai-rtos-integration",
                    head_repository_id=999,
                ),
                fake_run(
                    run_id=104,
                    run_number=103,
                    event="push",
                    head_branch="feat/axvisor-ai-rtos-integration",
                    head_repository_id=1329374417,
                ),
                fake_run(
                    run_id=105,
                    run_number=200,
                    event="pull_request",
                    head_branch="feat/axvisor-ai-rtos-integration",
                    head_repository_id=1329374417,
                ),
                fake_run(
                    run_id=106,
                    run_number=104,
                    event="pull_request",
                    head_branch="feat/axvisor-ai-rtos-integration",
                    head_repository_id=1329374417,
                    pull_request_number=999,
                ),
            ],
        )

        cancelled_run_ids = cancelled_runs(result)
        self.assertEqual(cancelled_run_ids, {101})

    def test_pull_request_number_match_remains_supported(self) -> None:
        result = run_cancellation(
            event_name="pull_request",
            pr_number="2078",
            pr_head_ref="current-branch",
            pr_head_repository_id="42",
            runs=[
                fake_run(
                    run_id=107,
                    run_number=100,
                    event="pull_request",
                    head_branch="historical-branch-name",
                    head_repository_id=99,
                    pull_request_number=2078,
                )
            ],
        )

        self.assertEqual(cancelled_runs(result), {107})


class RouteResult:
    def __init__(
        self,
        output: str,
        summary: str,
        stdout: str,
        stderr: str,
        gh_calls: list[str],
    ):
        self.output = output
        self.summary = summary
        self.stdout = stdout
        self.stderr = stderr
        self.gh_calls = gh_calls

    @property
    def should_run(self) -> str:
        outputs = dict(line.split("=", maxsplit=1) for line in self.output.splitlines())
        return outputs["should_run"]


def run_route(
    *,
    event_name: str,
    head_repository: str = "rcore-os/tgoskits",
    push_runs: str = "",
    has_active_matrix: str = "false",
    active_matrix_run_ids: str = "",
    push_run_is_usable: str = "true",
    push_run_status: str = "completed",
    push_run_query_exit: str = "0",
    push_run_query_fail_ids: str = "",
    run_query_exit: str = "0",
    job_query_exit: str = "0",
    push_runs_after_query: int = 1,
) -> RouteResult:
    script = route_script()
    with tempfile.TemporaryDirectory() as temp_dir_name:
        temp_dir = Path(temp_dir_name)
        bin_dir = temp_dir / "bin"
        bin_dir.mkdir()
        fake_gh = bin_dir / "gh"
        fake_gh.write_text(FAKE_GH, encoding="utf-8")
        fake_gh.chmod(0o755)
        fake_sleep = bin_dir / "sleep"
        fake_sleep.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        fake_sleep.chmod(0o755)

        output_file = temp_dir / "output"
        summary_file = temp_dir / "summary"
        gh_log = temp_dir / "gh.log"
        env = os.environ.copy()
        env.update(
            {
                "EVENT_NAME": event_name,
                "GITHUB_OUTPUT": str(output_file),
                "GITHUB_REPOSITORY": "rcore-os/tgoskits",
                "GITHUB_STEP_SUMMARY": str(summary_file),
                "HEAD_REPOSITORY": head_repository,
                "HEAD_SHA": "fc2a957ef330a39ef673d7364db1909fdbfe2821",
                "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                "PR_HEAD_REF": "fix/qemu-forward-progress",
                "FAKE_ACTIVE_MATRIX_RUN_IDS": active_matrix_run_ids,
                "FAKE_GH_LOG": str(gh_log),
                "FAKE_HAS_ACTIVE_MATRIX": has_active_matrix,
                "FAKE_JOB_QUERY_EXIT": job_query_exit,
                "FAKE_PUSH_RUNS": push_runs,
                "FAKE_PUSH_RUN_IS_USABLE": push_run_is_usable,
                "FAKE_PUSH_RUN_STATUS": push_run_status,
                "FAKE_PUSH_RUN_QUERY_EXIT": push_run_query_exit,
                "FAKE_PUSH_RUN_QUERY_FAIL_IDS": push_run_query_fail_ids,
                "FAKE_RUN_QUERY_EXIT": run_query_exit,
                "FAKE_PUSH_RUNS_AFTER_QUERY": str(push_runs_after_query),
                "FAKE_STATE_DIR": str(temp_dir),
            }
        )
        completed = subprocess.run(
            ["bash", "-c", script],
            cwd=WORKSPACE_ROOT,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        if completed.returncode != 0:
            raise AssertionError(
                f"route script failed with {completed.returncode}:\n{completed.stderr}"
            )
        return RouteResult(
            output_file.read_text(encoding="utf-8"),
            summary_file.read_text(encoding="utf-8")
            if summary_file.exists()
            else "",
            completed.stdout,
            completed.stderr,
            gh_log.read_text(encoding="utf-8").splitlines()
            if gh_log.exists()
            else [],
        )


def route_script() -> str:
    return workflow_step_script("Route duplicate events")


def run_cancellation(
    *,
    event_name: str = "push",
    ref_name: str = "fix/qemu-forward-progress",
    pr_number: str = "",
    pr_head_ref: str = "",
    pr_head_repository_id: str = "",
    runs: list[dict[str, object]] | None = None,
) -> RouteResult:
    script = workflow_step_script("Cancel older queued or running runs")
    if runs is None:
        runs = [
            fake_run(
                run_id=101,
                run_number=100,
                event="push",
                head_branch=ref_name,
                head_repository_id=1,
            )
        ]
    with tempfile.TemporaryDirectory() as temp_dir_name:
        temp_dir = Path(temp_dir_name)
        bin_dir = temp_dir / "bin"
        bin_dir.mkdir()
        fake_gh = bin_dir / "gh"
        fake_gh.write_text(FAKE_CANCEL_GH, encoding="utf-8")
        fake_gh.chmod(0o755)
        fake_sleep = bin_dir / "sleep"
        fake_sleep.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        fake_sleep.chmod(0o755)

        gh_log = temp_dir / "gh.log"
        env = os.environ.copy()
        env.update(
            {
                "CURRENT_RUN_NUMBER": "200",
                "EVENT_NAME": event_name,
                "FAKE_CANCEL_RUNS": json.dumps(runs),
                "FAKE_GH_LOG": str(gh_log),
                "FAKE_RECHECK_STATUS": "queued",
                "GITHUB_REPOSITORY": "rcore-os/tgoskits",
                "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                "PR_HEAD_REF": pr_head_ref,
                "PR_HEAD_REPOSITORY_ID": pr_head_repository_id,
                "PR_NUMBER": pr_number,
                "REF_NAME": ref_name,
            }
        )
        completed = subprocess.run(
            ["bash", "-c", script],
            cwd=WORKSPACE_ROOT,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        if completed.returncode != 0:
            raise AssertionError(
                f"cancellation script failed with {completed.returncode}:\n"
                f"{completed.stderr}"
            )
        return RouteResult(
            "",
            "",
            completed.stdout,
            completed.stderr,
            gh_log.read_text(encoding="utf-8").splitlines(),
        )


def fake_run(
    *,
    run_id: int,
    run_number: int,
    event: str,
    head_branch: str,
    head_repository_id: int,
    pull_request_number: int | None = None,
) -> dict[str, object]:
    pull_requests = (
        [] if pull_request_number is None else [{"number": pull_request_number}]
    )
    return {
        "event": event,
        "head_branch": head_branch,
        "head_repository": {"id": head_repository_id},
        "html_url": f"https://example.test/runs/{run_id}",
        "id": run_id,
        "pull_requests": pull_requests,
        "run_number": run_number,
        "status": "queued",
    }


def cancelled_runs(result: RouteResult) -> set[int]:
    return {
        int(call.split("actions/runs/", maxsplit=1)[1].split("/cancel", maxsplit=1)[0])
        for call in result.gh_calls
        if "/cancel" in call and "/force-cancel" not in call
    }


def workflow_step_script(step_name: str) -> str:
    return workflow_step_script_in(
        CI_WORKFLOW.read_text(encoding="utf-8"), step_name
    )


def workflow_step_script_in(workflow: str, step_name: str) -> str:
    step = named_step_block(workflow, step_name)
    lines = step.splitlines()
    run_index = next(
        index for index, line in enumerate(lines) if line.strip() == "run: |"
    )
    return textwrap.dedent("\n".join(lines[run_index + 1 :]))


def run_result_summary_step(
    script: str, **env_overrides: str
) -> subprocess.CompletedProcess:
    """Execute a workflow's result-summary step against a temporary summary.

    The step only reads stage results from the environment and appends to
    ``GITHUB_STEP_SUMMARY``, so a local bash run reproduces the workflow's exit
    status without touching any external state.
    """
    with tempfile.TemporaryDirectory() as temp_dir_name:
        summary = Path(temp_dir_name) / "summary"
        env = os.environ.copy()
        env.update(
            {
                "GITHUB_STEP_SUMMARY": str(summary),
                "TITLE": "fixture",
                "REVISION_LABEL": "tested revision",
                "PLAN_RESULT": "success",
                "CHECKS_RESULT": "success",
                "REVISION": "fixture",
            }
        )
        env.update(env_overrides)
        # GitHub runs `run:` steps with `bash -e`, so a failing `test` aborts
        # the step even though the script does not set errexit itself.
        return subprocess.run(
            ["bash", "-e", "-c", script],
            cwd=WORKSPACE_ROOT,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )


FAKE_GH = r'''#!/usr/bin/env python3
import os
import sys
from pathlib import Path


arguments = " ".join(sys.argv[1:])
with Path(os.environ["FAKE_GH_LOG"]).open("a", encoding="utf-8") as log:
    log.write(arguments + "\n")

if "actions/workflows/ci.yml/runs" in arguments:
    query_count_file = Path(os.environ["FAKE_STATE_DIR"]) / "run-query-count"
    query_count = (
        int(query_count_file.read_text(encoding="utf-8"))
        if query_count_file.exists()
        else 0
    ) + 1
    query_count_file.write_text(str(query_count), encoding="utf-8")
    if query_count >= int(os.environ["FAKE_PUSH_RUNS_AFTER_QUERY"]):
        print(os.environ["FAKE_PUSH_RUNS"])
    sys.exit(int(os.environ["FAKE_RUN_QUERY_EXIT"]))
if "/actions/runs/" in arguments and "/jobs" in arguments:
    run_id = arguments.split("/actions/runs/", maxsplit=1)[1].split("/", maxsplit=1)[0]
    active_run_ids = os.environ["FAKE_ACTIVE_MATRIX_RUN_IDS"].split(",")
    if run_id in active_run_ids or os.environ["FAKE_HAS_ACTIVE_MATRIX"] == "true":
        print("501")
    sys.exit(int(os.environ["FAKE_JOB_QUERY_EXIT"]))
if "/actions/runs/" in arguments:
    run_id = arguments.split("/actions/runs/", maxsplit=1)[1].split()[0]
    print(
        f'{os.environ["FAKE_PUSH_RUN_STATUS"]}\t'
        f'{os.environ["FAKE_PUSH_RUN_IS_USABLE"]}'
    )
    if run_id in os.environ["FAKE_PUSH_RUN_QUERY_FAIL_IDS"].split(","):
        sys.exit(1)
    sys.exit(int(os.environ["FAKE_PUSH_RUN_QUERY_EXIT"]))

print(f"unexpected gh invocation: {arguments}", file=sys.stderr)
sys.exit(2)
'''


FAKE_CANCEL_GH = r'''#!/usr/bin/env python3
import json
import os
import re
import subprocess
import sys
from pathlib import Path


arguments = " ".join(sys.argv[1:])
with Path(os.environ["FAKE_GH_LOG"]).open("a", encoding="utf-8") as log:
    log.write(arguments + "\n")

if "actions/workflows/ci.yml/runs?status=" in arguments:
    status = re.search(r"status=([^& ]+)", arguments).group(1)
    runs = [
        run
        for run in json.loads(os.environ["FAKE_CANCEL_RUNS"])
        if run["status"] == status
    ]
    jq_index = sys.argv.index("--jq")
    completed = subprocess.run(
        ["jq", "-r", sys.argv[jq_index + 1]],
        input=json.dumps({"workflow_runs": runs}),
        capture_output=True,
        text=True,
        check=False,
    )
    sys.stdout.write(completed.stdout)
    sys.stderr.write(completed.stderr)
    sys.exit(completed.returncode)

cancel_match = re.search(r"actions/runs/(\d+)/(?:force-)?cancel", arguments)
if cancel_match:
    sys.exit(0)

run_match = re.search(r"actions/runs/(\d+)", arguments)
if run_match:
    print(os.environ["FAKE_RECHECK_STATUS"])
    sys.exit(0)

print(f"unexpected gh invocation: {arguments}", file=sys.stderr)
sys.exit(2)
'''


if __name__ == "__main__":
    unittest.main()
