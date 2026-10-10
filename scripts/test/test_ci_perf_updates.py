#!/usr/bin/env python3

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).parent))
import ci_perf_updates


def write_report(directory: Path, name: str, payload: object) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    (directory / name).write_text(json.dumps(payload), encoding="utf-8")


class PerformanceUpdateCollectionTests(unittest.TestCase):
    def test_no_included_sources_is_an_empty_update(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            results = ci_perf_updates.collect_updates([], Path(temporary) / "updates")
            self.assertEqual(results, [])

    def test_missing_sources_are_empty_and_do_not_create_bridge_files(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "updates"
            results = ci_perf_updates.collect_updates(
                [("axvisor", root / "missing"), ("starry", root / "also-missing")],
                output,
            )

            self.assertEqual([result.count for result in results], [0, 0])
            self.assertEqual(list(output.glob("*.json")), [])

    def test_reports_are_sorted_and_sources_remain_isolated(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            axvisor = root / "axvisor"
            starry = root / "starry"
            write_report(axvisor, "z.json", [{"name": "z", "unit": "cycles", "value": 2}])
            write_report(axvisor, "a.json", [{"name": "a", "unit": "cycles", "value": 1}])
            write_report(starry, "one.json", [{"name": "s", "unit": "MiB/s", "value": 3}])

            results = ci_perf_updates.collect_updates(
                [("axvisor", axvisor), ("starry", starry)], root / "updates"
            )

            self.assertEqual([result.count for result in results], [2, 1])
            self.assertEqual(
                [item["name"] for item in json.loads((root / "updates/axvisor.json").read_text())],
                ["a", "z"],
            )
            self.assertEqual(
                json.loads((root / "updates/starry.json").read_text())[0]["name"], "s"
            )

    def test_malformed_report_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            reports = root / "reports"
            reports.mkdir()
            (reports / "broken.json").write_text("{", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "invalid performance report"):
                ci_perf_updates.load_report_metrics(reports)

            (reports / "broken.json").unlink()
            write_report(reports, "object.json", {"name": "bad"})
            with self.assertRaisesRegex(ValueError, "must contain a JSON array"):
                ci_perf_updates.load_report_metrics(reports)

    def test_metric_shape_is_checked_before_upload(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            reports = Path(temporary) / "reports"
            write_report(reports, "bad.json", [{"name": "x", "unit": "cycles", "value": "1"}])
            with self.assertRaises(ValueError):
                ci_perf_updates.load_report_metrics(reports)

    def test_source_names_and_duplicates_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaises(ValueError):
                ci_perf_updates.collect_updates([("../axvisor", root)], root / "updates")
            with self.assertRaises(ValueError):
                ci_perf_updates.collect_updates(
                    [("axvisor", root), ("axvisor", root)], root / "updates"
                )

    def test_main_writes_workflow_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            reports = root / "reports"
            write_report(reports, "one.json", [{"name": "x", "unit": "cycles", "value": 1}])
            output = root / "github-output"
            with mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(output)}, clear=False):
                self.assertEqual(
                    ci_perf_updates.main(
                        ["--output-dir", str(root / "updates"), "--source", f"axvisor={reports}"]
                    ),
                    0,
                )
            workflow_output = output.read_text(encoding="utf-8")
            self.assertIn("has_updates=true", workflow_output)
            self.assertIn("date=", workflow_output)
            self.assertIn('counts={"axvisor":1}', workflow_output)


if __name__ == "__main__":
    unittest.main()
