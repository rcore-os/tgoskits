#!/usr/bin/env python3

import importlib.util
import re
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from typing import Any

MODULE_PATH = Path(__file__).with_name("ci_plan.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("ci_plan", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ci_plan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_plan)

# Keep matrix assertions tied to the planner's registered capabilities.  A new
# test group therefore does not require updating this test's fixture list.
MAIN_TEST_GROUPS = tuple(ci_plan.TEST_GROUP_OUTPUT_PREFIXES)
MAIN_TEST_PREFIXES = tuple(ci_plan.TEST_GROUP_OUTPUT_PREFIXES.values())


def main_test_rows(plan: dict) -> list[dict]:
    return [
        row
        for prefix in MAIN_TEST_PREFIXES
        for row in plan[f"{prefix}_matrix"]["include"]
    ]


class CiPlanTests(unittest.TestCase):
    def test_starry_apps_plan_excludes_performance_checks(self):
        benchmark_ids = {
            check["id"]
            for check in ci_plan.load_catalog((ci_plan.BENCHMARK_MANIFEST,))
        }
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="schedule",
        )
        plan = ci_plan.build_starry_apps_plan(context)

        self.assertEqual(set(plan), {"starry_apps_matrix"})
        rows = plan["starry_apps_matrix"]["include"]
        self.assertTrue(rows)
        self.assertTrue(all(row["group"] == "Starry Apps" for row in rows))
        self.assertTrue({row["id"] for row in rows}.isdisjoint(benchmark_ids))
        self.assertFalse(any(row["performance_report"] for row in rows))

    def test_axvisor_nightly_plan_excludes_performance_checks(self):
        benchmark_ids = {
            check["id"]
            for check in ci_plan.load_catalog((ci_plan.BENCHMARK_MANIFEST,))
        }
        for event in ("schedule", "workflow_dispatch"):
            with self.subTest(event=event):
                context = ci_plan.PlanContext(
                    repository="rcore-os/tgoskits",
                    repository_owner="rcore-os",
                    event_name=event,
                )
                plan = ci_plan.build_axvisor_nightly_plan(context)
                rows = plan["axvisor_matrix"]["include"]

                self.assertEqual(set(plan), {"axvisor_matrix"})
                self.assertTrue(rows)
                self.assertTrue({row["id"] for row in rows}.isdisjoint(benchmark_ids))
                self.assertFalse(any(row["performance_report"] for row in rows))
                self.assertTrue(all(row["group"] == "AxVisor" for row in rows))

                # The default main CI matrix never schedules nightly or benchmark rows.
                main = ci_plan.build_main_plan(context)
                main_ids = {
                    row["id"] for row in main["axvisor_matrix"]["include"]
                }
                self.assertTrue(main_ids)
                self.assertTrue(main_ids.isdisjoint({row["id"] for row in rows}))

    def test_benchmarks_plan_owns_every_benchmark_matrix(self):
        checks = ci_plan.load_catalog((ci_plan.BENCHMARK_MANIFEST,))
        expected_ids = {check["id"] for check in checks}
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="schedule",
        )

        plan = ci_plan.build_benchmarks_plan(context)

        self.assertEqual(
            set(plan),
            {
                "prepare_matrix",
                "axvisor_performance_matrix",
                "starry_performance_matrix",
                "starry_board_performance_matrix",
            },
        )
        axvisor_rows = plan["axvisor_performance_matrix"]["include"]
        starry_qemu_rows = plan["starry_performance_matrix"]["include"]
        starry_board_rows = plan["starry_board_performance_matrix"]["include"]
        self.assertTrue(axvisor_rows)
        self.assertTrue(starry_qemu_rows)
        self.assertTrue(starry_board_rows)

        rows = axvisor_rows + starry_qemu_rows + starry_board_rows
        self.assertEqual(len(rows), len(expected_ids))
        self.assertEqual({row["id"] for row in rows}, expected_ids)
        self.assertTrue(all(row["performance_report"] for row in rows))
        self.assertTrue(
            all(
                row["group"] == ci_plan.AXVISOR_BENCHMARK_GROUP
                for row in axvisor_rows
            )
        )
        starry_rows = starry_qemu_rows + starry_board_rows
        self.assertTrue(
            all(
                row["group"] == ci_plan.STARRY_APPS_BENCHMARK_GROUP
                for row in starry_rows
            )
        )
        self.assertTrue(
            all("board" not in row["runs_on"] for row in starry_qemu_rows)
        )
        self.assertTrue(
            all("board" in row["runs_on"] for row in starry_board_rows)
        )
        self.assertTrue(
            all(row["download_xtask_bin_artifact"] for row in starry_rows)
        )
        self.assertTrue(
            all(
                row["performance_artifact_prefix"] == "axvisor-nightly-performance"
                for row in axvisor_rows
            )
        )
        self.assertTrue(
            all(
                row["performance_artifact_prefix"]
                == "starry-apps-nightly-performance"
                for row in starry_rows
            )
        )
        producer, = plan["prepare_matrix"]["include"]
        self.assertTrue(producer["upload_xtask_bin_artifact"])
        self.assertEqual(producer["command"], "cargo build -p tg-xtask")
        for row in starry_rows:
            self.assertEqual(
                row["xtask_bin_artifact_name"],
                producer["xtask_bin_artifact_name"],
            )

        with self.assertRaises(ci_plan.PlanError):
            ci_plan.build_benchmarks_plan(
                ci_plan.replace(context, event_name="pull_request")
            )

    def test_benchmark_manifest_auto_enables_nightly_and_reports(self):
        checks = ci_plan.load_catalog((ci_plan.BENCHMARK_MANIFEST,))
        self.assertTrue(checks)
        for check in checks:
            self.assertTrue(check["nightly_only"])
            self.assertTrue(check["performance_report"])

    def test_axvisor_nightly_manifest_is_nightly_only_without_reports(self):
        checks = ci_plan.load_catalog((ci_plan.AXVISOR_NIGHTLY_MANIFEST,))
        self.assertTrue(checks)
        for check in checks:
            self.assertTrue(check["nightly_only"])
            self.assertFalse(check.get("performance_report", False))

    def test_check_manifests_do_not_declare_removed_booleans(self):
        for manifest in sorted(ci_plan.CHECKS_ROOT.rglob("*.toml")):
            with self.subTest(manifest=manifest.name):
                document = tomllib.loads(manifest.read_text(encoding="utf-8"))
                for check in document.get("check", []):
                    self.assertNotIn("nightly_only", check)
                    self.assertNotIn("performance_report", check)

    def test_main_ci_never_runs_axvisor_nightly_only_cases(self):
        nightly_ids = {
            check["id"]
            for manifest in (ci_plan.AXVISOR_NIGHTLY_MANIFEST, ci_plan.BENCHMARK_MANIFEST)
            for check in ci_plan.load_catalog((manifest,))
        }
        for event in ("pull_request", "push", "workflow_dispatch", "schedule"):
            with self.subTest(event=event):
                context = ci_plan.replace(self.upstream, event_name=event)
                rows = main_test_rows(ci_plan.build_main_plan(context))
                self.assertTrue(rows)
                self.assertTrue({row["id"] for row in rows}.isdisjoint(nightly_ids))
                self.assertTrue(all(not row.get("nightly_only", False) for row in rows))

    def test_functional_smoke_path_does_not_route_to_the_nightly_benchmark_check(
        self,
    ):
        path = "apps/starry/qemu/compile-sim-bench/qemu-x86_64.toml"
        context = ci_plan.replace(
            self.upstream,
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                ignored_apps=(path,),
            ),
        )

        plan = ci_plan.build_main_plan(context)

        self.assertEqual(plan["starry_matrix"]["include"], [])
        self.assertFalse(plan["starry_required"])

    def test_axvisor_nightly_rejects_incremental_pr_mode(self):
        with self.assertRaises(ci_plan.PlanError):
            ci_plan.build_axvisor_nightly_plan(self.upstream)

    def test_axvisor_nightly_preserves_runner_owner_restrictions(self):
        context = ci_plan.PlanContext(
            repository="example/tgoskits",
            repository_owner="example",
            event_name="workflow_dispatch",
        )
        rows = ci_plan.build_axvisor_nightly_plan(context)["axvisor_matrix"]["include"]
        self.assertTrue(rows)
        self.assertTrue(all("self-hosted" not in row["runs_on"] for row in rows))

    def setUp(self) -> None:
        self.upstream = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
        )

    def test_main_plan_has_unique_checks_in_required_groups(
        self,
    ) -> None:
        plan = ci_plan.build_main_plan(self.upstream)

        self.assertTrue(plan["static_required"])
        static_rows = self.assert_unique_ids(plan["static_matrix"]["include"])
        test_rows = self.assert_unique_ids(main_test_rows(plan))
        self.assertNotIn("test_matrix", plan)
        self.assertTrue(static_rows.keys().isdisjoint(test_rows))
        for prefix, group in zip(MAIN_TEST_PREFIXES, MAIN_TEST_GROUPS, strict=True):
            group_rows = plan[f"{prefix}_matrix"]["include"]
            self.assertTrue(plan[f"{prefix}_required"])
            self.assertTrue(group_rows)
            self.assertTrue(all(row["group"] == group for row in group_rows))
        self.assertTrue(
            all(
                not row["name"].startswith(f"{row['group']} / ")
                for row in test_rows.values()
            )
        )

    def test_pull_request_crate_impact_selects_every_check_for_matching_os(
        self,
    ) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("os/arceos/modules/axhal/src/lib.rs",),
                changed_packages=("ax-hal",),
                affected_packages=("ax-hal",),
                affected_oses=("arceos",),
                targets=tuple(
                    f"arceos:{arch}"
                    for arch in ("aarch64", "x86_64", "riscv64", "loongarch64")
                ),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(main_test_rows(plan))

        self.assert_selects_full_group(rows, "Workspace")
        self.assert_selects_full_group(rows, "ArceOS")
        self.assertEqual(
            {row["group"] for row in rows.values()},
            {"Workspace", "ArceOS"},
        )
        self.assertTrue(plan["workspace_required"])
        self.assertTrue(plan["arceos_required"])
        self.assertFalse(plan["starry_required"])
        self.assertFalse(plan["axvisor_required"])

    def test_pull_request_impact_package_selects_standalone_check(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("bootloader/axloader/src/main.rs",),
                changed_packages=("axloader",),
                affected_packages=("axloader",),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        ids = {row["id"] for row in main_test_rows(plan)}
        expected = {
            check["id"]
            for check in ci_plan.load_catalog(ci_plan.MAIN_MANIFESTS)
            if "axloader" in check.get("impact_packages", [])
        }
        self.assertTrue(expected)
        self.assertTrue(expected <= ids)
        self.assertIn("Workspace", {row["group"] for row in main_test_rows(plan)})
        self.assertTrue(
            all(row["group"] in {"Workspace", "AxVisor"} for row in main_test_rows(plan))
        )

    def test_incremental_pr_uses_std_since_but_full_pr_does_not(self) -> None:
        incremental = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("components/shared/src/lib.rs",),
            ),
        )
        full = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact.full_selection("fixture"),
        )

        incremental_rows = self.assert_unique_ids(
            main_test_rows(ci_plan.build_main_plan(incremental))
        )
        full_rows = self.assert_unique_ids(
            main_test_rows(ci_plan.build_main_plan(full))
        )

        incremental_std = [
            row for row in incremental_rows.values() if '--since "$SINCE_REF"' in row["command"]
        ]
        full_std = [
            row
            for row in full_rows.values()
            if row["id"] == "test-with-std" and "--since" in row["command"]
        ]
        self.assertTrue(incremental_std)
        self.assertFalse(full_std)

    def test_app_only_impact_does_not_select_runtime_checks(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="only ignored app paths changed",
                changed_paths=("apps/starry/demo/main.c",),
                ignored_apps=("apps/starry/demo/main.c",),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(main_test_rows(plan))

        self.assert_selects_full_group(rows, "Workspace")
        self.assertEqual({row["group"] for row in rows.values()}, {"Workspace"})
        self.assertTrue(plan["workspace_required"])
        for prefix in ("arceos", "starry", "axvisor"):
            self.assertFalse(plan[f"{prefix}_required"])
            self.assertEqual(plan[f"{prefix}_matrix"]["include"], [])

    def test_non_pr_events_ignore_impact_and_preserve_full_matrix(self) -> None:
        impact = ci_plan.CiImpact(
            full=False,
            reason="must be ignored outside pull requests",
            changed_paths=("components/axcpu/src/arch/aarch64/mod.rs",),
            targets=("axvisor:aarch64",),
        )
        for event_name in ("push", "workflow_dispatch"):
            with self.subTest(event=event_name):
                baseline = ci_plan.build_main_plan(
                    ci_plan.PlanContext(
                        repository="rcore-os/tgoskits",
                        repository_owner="rcore-os",
                        event_name=event_name,
                    )
                )
                with_impact = ci_plan.build_main_plan(
                    ci_plan.PlanContext(
                        repository="rcore-os/tgoskits",
                        repository_owner="rcore-os",
                        event_name=event_name,
                        impact=impact,
                    )
                )

                self.assertEqual(with_impact, baseline)
                self.assert_unique_ids(main_test_rows(with_impact))

    def test_generic_driver_suite_routes_source_and_rejects_missing_cases(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            case = root / "test-suit/arceos/drivers/packet-link"
            (case / "src").mkdir(parents=True)
            (case / "build-x86_64-unknown-none.toml").write_text("features = []\n")
            (case / "qemu-x86_64.toml").write_text("args = []\n")
            source = "test-suit/arceos/drivers/packet-link/src/main.rs"
            (root / source).write_text("fn main() {}\n")
            registration = {"kind": "arceos-qemu", "arch": "x86_64", "group": "drivers"}
            checks = [{"id": "driver-suite", "name": "drivers", "suite": [registration]}]

            ci_plan._validate_suite_registrations([registration], "synthetic")
            with self.assertRaisesRegex(ci_plan.PlanError, "unsupported"):
                ci_plan._validate_suite_registrations(
                    [{**registration, "group": []}], "synthetic"
                )
            ci_plan.validate_suite_catalog(root, checks)
            selection, = ci_plan.resolve_suite_selections(root, checks, [source])
            self.assertEqual(selection.template_id, "driver-suite")
            self.assertEqual(
                selection.command,
                "cargo xtask arceos test qemu --arch x86_64 "
                "--test-group drivers --test-case packet-link",
            )
            registration["cases"] = ["missing-endpoint"]
            with self.assertRaisesRegex(ci_plan.SuiteRouteError, "missing"):
                ci_plan.validate_suite_catalog(root, checks)

    def test_unregistered_test_suite_fails_planning(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(
                    "test-suit/starryos/board-fixture/boot/board-fixture.toml",
                ),
                test_suite_paths=(
                    "test-suit/starryos/board-fixture/boot/board-fixture.toml",
                ),
                exclusive=True,
            ),
        )

        with self.assertRaisesRegex(ci_plan.PlanError, "not registered"):
            ci_plan.build_main_plan(context)

    def test_unsupported_test_group_fails_planning(self) -> None:
        with self.assertRaisesRegex(
            ci_plan.PlanError,
            "unsupported group 'Future OS'",
        ):
            ci_plan._build_test_group_outputs(
                [{"id": "future-os-check", "group": "Future OS"}]
            )

    def test_manifest_rejects_unsupported_test_group(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            manifest = Path(temp_dir) / "future-os.toml"
            manifest.write_text(
                """\
schema_version = 3
phase = "test"
group = "Future OS"

[[check]]
id = "future-os-check"
name = "Future OS check"
command = "true"
""",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(
                ci_plan.PlanError,
                "unsupported test group 'Future OS'",
            ):
                ci_plan._load_manifest(manifest)

    def test_suite_plus_os_wide_crate_uses_the_broader_os_checks(self) -> None:
        # The suite path is deliberately synthetic: because the OS-wide crate
        # impact already covers Starry, planner routing must not depend on a
        # particular registered case being present.
        path = "test-suit/starryos/qemu/fixture/qemu-aarch64.toml"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path, "components/shared/src/lib.rs"),
                changed_packages=("shared",),
                affected_packages=("shared", "starryos"),
                affected_oses=("starry",),
                test_suite_paths=(path,),
                targets=tuple(
                    f"starry:{arch}"
                    for arch in ("aarch64", "x86_64", "riscv64", "loongarch64")
                ),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(main_test_rows(plan))

        self.assertTrue(plan["static_required"])
        self.assert_selects_full_group(rows, "Starry")
        self.assertFalse(any(check_id.startswith("suite-") for check_id in rows))

    def test_unmatched_known_os_input_falls_back_to_that_os(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("os/StarryOS/configs/board/future-board.toml",),
                input_selections=("starry:board:future-board",),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(main_test_rows(plan))

        self.assert_selects_full_group(rows, "Starry")
        self.assertFalse(
            any(row["group"] in {"ArceOS", "AxVisor"} for row in rows.values())
        )
        self.assertEqual(plan["arceos_matrix"]["include"], [])
        self.assertEqual(plan["axvisor_matrix"]["include"], [])

    def test_fork_repository_filters_owner_checks_and_falls_back_from_qcs(
        self,
    ) -> None:
        context = ci_plan.PlanContext(
            repository="contributor/tgoskits",
            repository_owner="contributor",
            event_name="push",
        )
        plan = ci_plan.build_main_plan(context)
        static_rows = self.assert_unique_ids(plan["static_matrix"]["include"])
        test_rows = self.assert_unique_ids(main_test_rows(plan))

        self.assertTrue(test_rows)
        self.assertTrue(any(row["group"] == "Workspace" for row in test_rows.values()))
        self.assertFalse(any("board" in row["runs_on"] for row in test_rows.values()))
        self.assertTrue(
            all(
                row["runs_on"] == ["ubuntu-latest"]
                for row in (*static_rows.values(), *test_rows.values())
            )
        )
        self.assertTrue(any(row["container_image"].startswith("ghcr.io/contributor/") for row in static_rows.values()))
        self.assertTrue(any(row["download_xtask_bin_artifact"] for row in test_rows.values()))

    def test_fork_pull_request_never_allocates_self_hosted_runners(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="contributor/tgoskits",
            base_ref="dev",
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(
            plan["static_matrix"]["include"] + main_test_rows(plan)
        )

        self.assertTrue(rows)
        self.assertTrue(
            all("self-hosted" not in row["runs_on"] for row in rows.values())
        )
        catalog = ci_plan.load_catalog(ci_plan.MAIN_MANIFESTS)
        self_hosted_only_ids = {
            check["id"]
            for check in catalog
            if "self-hosted" in check["runs_on"]
            and "fallback_environment" not in check
        }
        self.assertTrue(self_hosted_only_ids.isdisjoint(rows))
        self.assertEqual(rows["check-formatting"]["runs_on"], ["ubuntu-latest"])
        self.assertEqual(rows["run-clippy"]["runs_on"], ["ubuntu-latest"])

        board_path = next(
            path.relative_to(MODULE_PATH.parents[2]).as_posix()
            for path in (MODULE_PATH.parents[2] / "test-suit/arceos").glob(
                "board-*/*/*.toml"
            )
        )
        board_only = ci_plan.replace(
            context,
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(board_path,),
                test_suite_paths=(board_path,),
                exclusive=True,
            ),
        )
        board_plan = ci_plan.build_main_plan(board_only)
        board_rows = board_plan["static_matrix"]["include"] + main_test_rows(
            board_plan
        )
        self.assertTrue(board_rows)
        self.assertTrue(
            all("self-hosted" not in row["runs_on"] for row in board_rows)
        )

    def test_same_repository_pull_request_keeps_self_hosted_runners(self) -> None:
        plan = ci_plan.build_main_plan(self.upstream)
        rows = self.assert_unique_ids(
            plan["static_matrix"]["include"] + main_test_rows(plan)
        )

        self.assertTrue(any("self-hosted" in row["runs_on"] for row in rows.values()))

    def test_board_checks_declare_a_resource_group_and_qemu_does_not(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="schedule",
        )
        main_rows = main_test_rows(ci_plan.build_main_plan(context))
        nightly_rows = ci_plan.build_axvisor_nightly_plan(context)["axvisor_matrix"][
            "include"
        ]
        benchmark_plan = ci_plan.build_benchmarks_plan(context)
        benchmark_rows = (
            benchmark_plan["axvisor_performance_matrix"]["include"]
            + benchmark_plan["starry_performance_matrix"]["include"]
            + benchmark_plan["starry_board_performance_matrix"]["include"]
        )
        catalog = {
            check["id"]: check
            for check in ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS)
        }
        for row in (*main_rows, *nightly_rows, *benchmark_rows):
            boards = {
                registration["board"]
                for registration in catalog[row["id"]].get("suite", ())
                if "board" in registration
            }
            if boards:
                self.assertIn("board", row["runs_on"])
                if row["resource_group"]:
                    self.assertTrue(row["resource_group"])
            else:
                self.assertNotIn("board", row["runs_on"])
                self.assertEqual(row["resource_group"], "")

    def test_event_and_boolean_input_select_checks_independently(self) -> None:
        check = {"events": ["schedule"], "enable_boolean_input": "run_optional"}
        for event, enabled, expected in (
            ("schedule", frozenset(), True),
            ("workflow_dispatch", frozenset(), False),
            ("workflow_dispatch", frozenset({"run_optional"}), True),
            ("workflow_dispatch", frozenset({"other_input"}), False),
        ):
            with self.subTest(event=event, enabled=enabled):
                context = ci_plan.PlanContext(
                    repository="example/project",
                    repository_owner="example",
                    event_name=event,
                    enabled_boolean_inputs=enabled,
                )
                self.assertEqual(ci_plan._is_enabled(check, context), expected)

    def assert_unique_ids(
        self, rows: list[dict[str, Any]]
    ) -> dict[str, dict[str, Any]]:
        ids = [row["id"] for row in rows]
        self.assertEqual(
            len(ids),
            len(set(ids)),
            f"matrix check IDs must be unique: {ids}",
        )
        return {row["id"]: row for row in rows}

    def assert_selects_full_group(
        self, rows: dict[str, dict[str, Any]], group: str
    ) -> None:
        full_rows = self.assert_unique_ids(
            main_test_rows(ci_plan.build_main_plan(self.upstream))
        )
        selected_ids = {
            check_id for check_id, row in rows.items() if row["group"] == group
        }
        full_ids = {
            check_id for check_id, row in full_rows.items() if row["group"] == group
        }
        self.assertEqual(selected_ids, full_ids)


if __name__ == "__main__":
    unittest.main()
