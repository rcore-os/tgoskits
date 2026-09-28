#!/usr/bin/env python3

import importlib.util
import json
import tempfile
import unittest
import urllib.error
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("ci_perf_pages.py")
SPEC = importlib.util.spec_from_file_location("ci_perf_pages", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ci_perf_pages = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_perf_pages)


def remote(status: int, body: bytes | None) -> object:
    return ci_perf_pages.RemoteFile(status=status, content=body)


def fetch_map(mapping: dict[str, object]):
    def fetch(base_url: str, filename: str, buster: str):
        value = mapping[filename]
        if isinstance(value, Exception):
            raise value
        return value

    return fetch


class FetchPublishedFileTests(unittest.TestCase):
    def _opener(self, status: int, body: bytes):
        captured: dict[str, object] = {}

        class Response:
            def __init__(self) -> None:
                self.status = status

            def read(self) -> bytes:
                return body

            def __enter__(self) -> "Response":
                return self

            def __exit__(self, *args: object) -> bool:
                return False

        def opener(request):
            captured["url"] = request.full_url
            captured["headers"] = {
                name.lower(): value for name, value in request.header_items()
            }
            return Response()

        return opener, captured

    def test_fetch_uses_cache_buster_and_no_cache_header(self) -> None:
        opener, captured = self._opener(200, b"history")

        result = ci_perf_pages.fetch_published_file(
            "https://example.test/pages/", "history.json", "42-7", opener=opener
        )

        self.assertEqual(result.status, 200)
        self.assertEqual(
            captured["url"],
            "https://example.test/pages/benchmark/history.json?cache_buster=42-7",
        )
        self.assertEqual(captured["headers"]["cache-control"], "no-cache")

    def test_fetch_rejects_empty_200(self) -> None:
        opener, _ = self._opener(200, b"")

        with self.assertRaises(ci_perf_pages.PagesError):
            ci_perf_pages.fetch_published_file(
                "https://example.test", "history.json", "1-1", opener=opener
            )

    def test_fetch_treats_404_as_missing(self) -> None:
        def opener(request):
            raise urllib.error.HTTPError(request.full_url, 404, "not found", {}, None)

        result = ci_perf_pages.fetch_published_file(
            "https://example.test", "index.html", "1-1", opener=opener
        )

        self.assertEqual(result.status, 404)
        self.assertIsNone(result.content)

    def test_fetch_fails_on_unexpected_http_status(self) -> None:
        def opener(request):
            raise urllib.error.HTTPError(request.full_url, 500, "boom", {}, None)

        with self.assertRaises(ci_perf_pages.PagesError):
            ci_perf_pages.fetch_published_file(
                "https://example.test", "index.html", "1-1", opener=opener
            )


class PrepareDashboardTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        self.output_dir = self.root / "docs" / "build"
        self.updates_dir = self.root / "updates"
        self.updates_dir.mkdir()

    def config(self, **overrides: object) -> object:
        values = {
            "base_url": "https://example.test/pages",
            "output_dir": self.output_dir,
            "updates_dir": self.updates_dir,
            "run_id": "42",
            "run_attempt": "7",
        }
        values.update(overrides)
        return ci_perf_pages.PagesConfig(**values)

    def unreachable_git(self):
        def git(args: list[str]):
            raise AssertionError(f"legacy branch must not be read: {args}")

        return git

    def test_published_double_200_is_preserved_verbatim(self) -> None:
        history = b'{"axvisor": [{"date": "2026-09-01"}]}\n'
        index = b"<html>published</html>\n"

        ci_perf_pages.prepare_dashboard(
            self.config(),
            fetch_file=fetch_map(
                {"history.json": remote(200, history), "index.html": remote(200, index)}
            ),
            git=self.unreachable_git(),
        )

        out = self.output_dir / "benchmark"
        self.assertEqual((out / "history.json").read_bytes(), history)
        self.assertEqual((out / "index.html").read_bytes(), index)

    def test_partial_published_state_does_not_read_legacy_branch(self) -> None:
        with self.assertRaises(ci_perf_pages.PagesError):
            ci_perf_pages.prepare_dashboard(
                self.config(),
                fetch_file=fetch_map(
                    {
                        "history.json": remote(200, b"{}"),
                        "index.html": remote(404, None),
                    }
                ),
                git=self.unreachable_git(),
            )

    def test_double_404_with_unavailable_legacy_fails(self) -> None:
        def missing_git(args: list[str]):
            return 1, b""

        with self.assertRaises(ci_perf_pages.PagesError):
            ci_perf_pages.prepare_dashboard(
                self.config(),
                fetch_file=fetch_map(
                    {
                        "history.json": remote(404, None),
                        "index.html": remote(404, None),
                    }
                ),
                git=missing_git,
            )

    def test_double_404_with_complete_legacy_is_copied(self) -> None:
        history = b'[{"date": "2026-09-01", "revision": "legacy", "metrics": []}]\n'
        index = b"<html>legacy</html>\n"

        def git(args: list[str]):
            if args[:2] == ["fetch", "--depth=1"]:
                return 0, b""
            if args == ["show", "FETCH_HEAD:history.json"]:
                return 0, history
            if args == ["show", "FETCH_HEAD:index.html"]:
                return 0, index
            raise AssertionError(args)

        ci_perf_pages.prepare_dashboard(
            self.config(),
            fetch_file=fetch_map(
                {
                    "history.json": remote(404, None),
                    "index.html": remote(404, None),
                }
            ),
            git=git,
        )

        out = self.output_dir / "benchmark"
        self.assertEqual((out / "history.json").read_bytes(), history)
        self.assertEqual((out / "index.html").read_bytes(), index)

    def test_fetch_failure_stops_preparation(self) -> None:
        with self.assertRaises(ci_perf_pages.PagesError):
            ci_perf_pages.prepare_dashboard(
                self.config(),
                fetch_file=fetch_map(
                    {"history.json": ci_perf_pages.PagesError("network down")}
                ),
                git=self.unreachable_git(),
            )

    def test_legacy_array_merges_starry_without_dropping_axvisor(self) -> None:
        legacy_history = json.dumps(
            [
                {
                    "date": "2026-09-01",
                    "revision": "legacy",
                    "metrics": [
                        {"name": "vcpu-perf/throughput", "unit": "ops/s", "value": 10.0}
                    ],
                }
            ]
        ).encode()
        (self.updates_dir / "starry.json").write_text(
            json.dumps(
                [{"name": "sysbench/cpu", "unit": "ops/s", "value": 20.0}]
            ),
            encoding="utf-8",
        )

        def git(args: list[str]):
            if args[:2] == ["fetch", "--depth=1"]:
                return 0, b""
            if args == ["show", "FETCH_HEAD:history.json"]:
                return 0, legacy_history
            if args == ["show", "FETCH_HEAD:index.html"]:
                return 0, b"<html>legacy</html>"
            raise AssertionError(args)

        ci_perf_pages.prepare_dashboard(
            self.config(
                benchmark_run_id="123",
                benchmark_revision="abc",
                benchmark_date="2026-09-20",
            ),
            fetch_file=fetch_map(
                {
                    "history.json": remote(404, None),
                    "index.html": remote(404, None),
                }
            ),
            git=git,
        )

        merged = json.loads(
            (self.output_dir / "benchmark" / "history.json").read_text(encoding="utf-8")
        )
        self.assertEqual(
            [entry["revision"] for entry in merged["axvisor"]], ["legacy"]
        )
        self.assertEqual(
            [entry["revision"] for entry in merged["starry"]], ["abc"]
        )
        self.assertEqual(merged["starry"][0]["date"], "2026-09-20")
        index = (self.output_dir / "benchmark" / "index.html").read_text(
            encoding="utf-8"
        )
        self.assertIn("Starry", index)

    def test_benchmark_dispatch_without_increment_fails(self) -> None:
        with self.assertRaises(ci_perf_pages.PagesError):
            ci_perf_pages.prepare_dashboard(
                self.config(
                    benchmark_run_id="123",
                    benchmark_revision="abc",
                    benchmark_date="2026-09-20",
                ),
                fetch_file=fetch_map(
                    {
                        "history.json": remote(200, b'{"axvisor": []}'),
                        "index.html": remote(200, b"<html>published</html>"),
                    }
                ),
                git=self.unreachable_git(),
            )


if __name__ == "__main__":
    unittest.main()
