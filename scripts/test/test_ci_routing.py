#!/usr/bin/env python3

import json
import os
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

from scripts.test.check_ci_routing import mapping_block, named_step_block


WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
CI_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci.yml"
REUSABLE_CHECK_MATRIX = (
    WORKSPACE_ROOT / ".github/workflows/reusable-check-matrix.yml"
)
PR_CLEANUP_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci-pr-cleanup.yml"


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

    def test_fork_cleanup_uses_trusted_target_context(self) -> None:
        self.assertTrue(
            PR_CLEANUP_WORKFLOW.is_file(),
            "fork PR cleanup needs a pull_request_target workflow with a write token",
        )
        workflow = PR_CLEANUP_WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("pull_request_target:", workflow)
        self.assertIn("actions: write", workflow)
        self.assertIn(
            '"repos/${GITHUB_REPOSITORY}/pulls/${PR_NUMBER}"',
            workflow,
        )
        self.assertIn(
            'if [ "$PR_EVENT_HEAD_SHA" != "$PR_HEAD_SHA" ]',
            workflow,
        )
        self.assertIn(".head_sha != env.PR_HEAD_SHA", workflow)
        self.assertIn(".pull_requests[]?", workflow)
        self.assertIn(".head_repository.id", workflow)
        self.assertIn("actions/runs/${run_id}/cancel", workflow)
        self.assertIn("actions/runs/${run_id}/force-cancel", workflow)
        self.assertNotIn("actions/checkout", workflow)
        self.assertNotIn("pull_request.head.repo.full_name", workflow)

    def test_target_cleanup_cancels_only_old_current_pr_heads(self) -> None:
        result = run_target_cancellation(
            runs=[
                fake_run(
                    run_id=201,
                    run_number=100,
                    event="pull_request",
                    head_branch="fork-branch",
                    head_repository_id=42,
                    head_sha="old-head",
                ),
                fake_run(
                    run_id=202,
                    run_number=101,
                    event="pull_request",
                    head_branch="fork-branch",
                    head_repository_id=42,
                    head_sha="current-head",
                ),
                fake_run(
                    run_id=203,
                    run_number=102,
                    event="pull_request",
                    head_branch="fork-branch",
                    head_repository_id=99,
                    head_sha="other-fork-head",
                ),
                fake_run(
                    run_id=204,
                    run_number=103,
                    event="pull_request",
                    head_branch="other-branch",
                    head_repository_id=42,
                    head_sha="other-branch-head",
                ),
                fake_run(
                    run_id=205,
                    run_number=104,
                    event="push",
                    head_branch="fork-branch",
                    head_repository_id=42,
                    head_sha="old-push-head",
                ),
            ]
        )

        self.assertEqual(cancelled_runs(result), {201})
        self.assertTrue(
            any("actions/runs/201/force-cancel" in call for call in result.gh_calls)
        )

    def test_delayed_target_event_does_not_cancel_newer_head(self) -> None:
        result = run_target_cancellation(event_head_sha="superseded-head")

        self.assertEqual(cancelled_runs(result), set())
        self.assertFalse(
            any("actions/workflows/ci.yml/runs" in call for call in result.gh_calls)
        )


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

    def test_waiting_and_requested_pushes_suppress_pull_request(self) -> None:
        for status in ("waiting", "requested"):
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

    def test_fork_pull_request_cancels_only_older_matching_head_runs(self) -> None:
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


def run_target_cancellation(
    *,
    event_head_sha: str = "current-head",
    runs: list[dict[str, object]] | None = None,
) -> RouteResult:
    script = workflow_step_script(
        "Cancel older queued or running runs",
        PR_CLEANUP_WORKFLOW,
    )
    if runs is None:
        runs = []
    with tempfile.TemporaryDirectory() as temp_dir_name:
        temp_dir = Path(temp_dir_name)
        bin_dir = temp_dir / "bin"
        bin_dir.mkdir()
        fake_gh = bin_dir / "gh"
        fake_gh.write_text(FAKE_TARGET_CANCEL_GH, encoding="utf-8")
        fake_gh.chmod(0o755)
        fake_sleep = bin_dir / "sleep"
        fake_sleep.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        fake_sleep.chmod(0o755)

        gh_log = temp_dir / "gh.log"
        env = os.environ.copy()
        env.update(
            {
                "FAKE_CANCEL_RUNS": json.dumps(runs),
                "FAKE_CURRENT_HEAD_REF": "fork-branch",
                "FAKE_CURRENT_HEAD_REPOSITORY_ID": "42",
                "FAKE_CURRENT_HEAD_SHA": "current-head",
                "FAKE_GH_LOG": str(gh_log),
                "GITHUB_REPOSITORY": "rcore-os/tgoskits",
                "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                "PR_EVENT_HEAD_SHA": event_head_sha,
                "PR_NUMBER": "2078",
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
                f"target cancellation script failed with {completed.returncode}:\n"
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
    head_sha: str = "old-head",
    pull_request_number: int | None = None,
) -> dict[str, object]:
    pull_requests = (
        [] if pull_request_number is None else [{"number": pull_request_number}]
    )
    return {
        "event": event,
        "head_branch": head_branch,
        "head_repository": {"id": head_repository_id},
        "head_sha": head_sha,
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


def workflow_step_script(
    step_name: str,
    workflow_path: Path = CI_WORKFLOW,
) -> str:
    workflow = workflow_path.read_text(encoding="utf-8")
    step = named_step_block(workflow, step_name)
    lines = step.splitlines()
    run_index = next(
        index for index, line in enumerate(lines) if line.strip() == "run: |"
    )
    return textwrap.dedent("\n".join(lines[run_index + 1 :]))


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


FAKE_TARGET_CANCEL_GH = r'''#!/usr/bin/env python3
import json
import os
import re
import subprocess
import sys
from pathlib import Path


arguments = " ".join(sys.argv[1:])
with Path(os.environ["FAKE_GH_LOG"]).open("a", encoding="utf-8") as log:
    log.write(arguments + "\n")

if "/pulls/" in arguments:
    print(
        f'{os.environ["FAKE_CURRENT_HEAD_SHA"]}\t'
        f'{os.environ["FAKE_CURRENT_HEAD_REF"]}\t'
        f'{os.environ["FAKE_CURRENT_HEAD_REPOSITORY_ID"]}'
    )
    sys.exit(0)

if "actions/workflows/ci.yml/runs" in arguments:
    status_argument = next(
        argument
        for argument in sys.argv[1:]
        if argument.startswith("status=")
    )
    status = status_argument.split("=", maxsplit=1)[1]
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
        env=os.environ,
    )
    sys.stdout.write(completed.stdout)
    sys.stderr.write(completed.stderr)
    sys.exit(completed.returncode)

cancel_match = re.search(r"actions/runs/(\d+)/(?:force-)?cancel", arguments)
if cancel_match:
    sys.exit(0)

run_match = re.search(r"actions/runs/(\d+)", arguments)
if run_match:
    print("queued")
    sys.exit(0)

print(f"unexpected gh invocation: {arguments}", file=sys.stderr)
sys.exit(2)
'''


if __name__ == "__main__":
    unittest.main()
