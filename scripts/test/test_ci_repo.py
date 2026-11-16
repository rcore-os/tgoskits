#!/usr/bin/env python3
"""Regression tests for repository batch command exit status."""

import csv
import importlib.util
import io
import subprocess
import sys
import tempfile
import unittest
from contextlib import ExitStack, contextmanager, redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock


_REPO_PATH = Path(__file__).parents[1] / "repo" / "repo.py"
_REPO_SPEC = importlib.util.spec_from_file_location("repo_under_test", _REPO_PATH)
if _REPO_SPEC is None or _REPO_SPEC.loader is None:
    raise RuntimeError(f"Unable to load {_REPO_PATH}")
repo = importlib.util.module_from_spec(_REPO_SPEC)
sys.modules[_REPO_SPEC.name] = repo
_REPO_SPEC.loader.exec_module(repo)


class RepoBatchExitStatusTests(unittest.TestCase):
    FAIL_URL = "https://example.invalid/fail.git"
    OK_URL = "https://example.invalid/ok.git"
    ERROR_CASES = (
        ("pull", "pull_subtree", ValueError),
        ("pull", "pull_subtree", subprocess.CalledProcessError),
        ("push", "push_subtree", ValueError),
        ("push", "push_subtree", subprocess.CalledProcessError),
        ("push", "detect_remote_branch", ValueError),
    )

    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.csv_path = Path(self.temp_dir.name) / "repos.csv"

    def _write_repos(self, rows):
        with self.csv_path.open("w", newline="", encoding="utf-8") as stream:
            writer = csv.writer(stream)
            writer.writerow(["url", "branch", "target_dir", "category", "description"])
            for url, branch, target_dir in rows:
                writer.writerow([url, branch, target_dir, "", ""])

    def _run_repo(self, command, *command_args):
        argv = [
            "repo.py",
            "--csv",
            str(self.csv_path),
            command,
            *command_args,
        ]
        with mock.patch.object(sys, "argv", argv):
            with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                return repo.main()

    @staticmethod
    def _make_error(error_type, message):
        if error_type is ValueError:
            return ValueError(message)
        return subprocess.CalledProcessError(1, ["git"], stderr=message)

    @contextmanager
    def _patch_action(self, command, side_effect):
        method_name = "pull_subtree" if command == "pull" else "push_subtree"
        with ExitStack() as stack:
            if command == "push":
                stack.enter_context(
                    mock.patch.object(
                        repo.GitSubtreeManager,
                        "detect_current_branch",
                        return_value="topic",
                    )
                )
            action = stack.enter_context(
                mock.patch.object(
                    repo.GitSubtreeManager,
                    method_name,
                    side_effect=side_effect,
                )
            )
            yield action

    def _assert_all_continues_after_error(self, command, method_name, error_type):
        first_branch = "" if method_name == "detect_remote_branch" else "main"
        self._write_repos(
            [
                (self.FAIL_URL, first_branch, "vendor/fail"),
                (self.OK_URL, "main", "vendor/ok"),
            ]
        )
        error = self._make_error(error_type, "batch operation failed")

        if method_name == "detect_remote_branch":
            with mock.patch.object(
                repo.GitSubtreeManager,
                "detect_remote_branch",
                side_effect=[error],
            ) as detect_remote_branch:
                with self._patch_action(command, [None]) as push_subtree:
                    result = self._run_repo(command, "--all")
            detect_remote_branch.assert_called_once_with(self.FAIL_URL, "vendor/fail")
            action = push_subtree
            expected_urls = [self.OK_URL]
        else:
            with self._patch_action(command, [error, None]) as action:
                result = self._run_repo(command, "--all")
            expected_urls = [self.FAIL_URL, self.OK_URL]

        self.assertEqual(result, 1)
        self.assertEqual(
            [call.args[0] for call in action.call_args_list],
            expected_urls,
        )

    def test_all_continues_after_repository_errors_and_returns_nonzero(self):
        for command, method_name, error_type in self.ERROR_CASES:
            with self.subTest(
                command=command,
                method=method_name,
                error=error_type.__name__,
            ):
                self._assert_all_continues_after_error(
                    command, method_name, error_type
                )

    def test_single_repository_error_returns_nonzero_and_stops(self):
        for command, method_name, error_type in self.ERROR_CASES:
            with self.subTest(
                command=command,
                method=method_name,
                error=error_type.__name__,
            ):
                first_branch = "" if method_name == "detect_remote_branch" else "main"
                self._write_repos(
                    [
                        (self.FAIL_URL, first_branch, "vendor/fail"),
                        (self.OK_URL, "main", "vendor/ok"),
                    ]
                )
                error = self._make_error(error_type, "single operation failed")

                if method_name == "detect_remote_branch":
                    with mock.patch.object(
                        repo.GitSubtreeManager,
                        "detect_remote_branch",
                        side_effect=[error],
                    ):
                        with self._patch_action(command, [None]) as push_subtree:
                            result = self._run_repo(command, "fail")
                    push_subtree.assert_not_called()
                else:
                    with self._patch_action(command, [error, None]) as action:
                        result = self._run_repo(command, "fail")
                    action.assert_called_once()
                    self.assertEqual(action.call_args.args[0], self.FAIL_URL)

                self.assertEqual(result, 1)

    def test_all_returns_zero_when_actions_succeed_or_are_skipped(self):
        repos = [
            (self.FAIL_URL, "main", "vendor/fail"),
            (self.OK_URL, "main", "vendor/ok"),
            ("https://example.invalid/skipped.git", "main", ""),
        ]
        for command in ("pull", "push"):
            with self.subTest(command=command):
                self._write_repos(repos)
                with self._patch_action(command, [None, None]) as action:
                    result = self._run_repo(command, "--all")
                self.assertEqual(result, 0)
                self.assertEqual(
                    [call.args[0] for call in action.call_args_list],
                    [self.FAIL_URL, self.OK_URL],
                )

    def test_all_without_repositories_returns_zero(self):
        self._write_repos([])
        for command in ("pull", "push"):
            with self.subTest(command=command):
                with self._patch_action(command, None) as action:
                    result = self._run_repo(command, "--all")
                self.assertEqual(result, 0)
                action.assert_not_called()


if __name__ == "__main__":
    unittest.main()
