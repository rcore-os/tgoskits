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
IMPACT_SPEC = importlib.util.spec_from_file_location(
    "ci_impact", MODULE_PATH.with_name("ci_impact.py")
)
assert IMPACT_SPEC is not None and IMPACT_SPEC.loader is not None
ci_impact = importlib.util.module_from_spec(IMPACT_SPEC)
IMPACT_SPEC.loader.exec_module(ci_impact)
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
                plan = ci_plan.build_main_plan(context)
                rows = main_test_rows(plan)
                self.assertTrue(rows)
                self.assertTrue({row["id"] for row in rows}.isdisjoint(nightly_ids))
                self.assertTrue(all(not row.get("nightly_only", False) for row in rows))
                axvisor_rows = plan["axvisor_matrix"]["include"]
                commands = "\n".join(row["command"] for row in axvisor_rows)
                self.assertNotIn("timer-stress", commands)
                self.assertNotIn("ivc-benchmark", commands)
                self.assertNotIn("orangepi-5-plus-vcpu-perf", commands)
                self.assertNotIn("--test-case ping", commands)
                self.assertIn(
                    "--board orangepi-5-plus-linux,orangepi-5-plus-starry --test-case smoke",
                    commands,
                )
                self.assertNotIn("--board orangepi-5-plus-linux\n", commands)
                self.assertIn("--test-case qemu-ivc", commands)
                self.assertIn(
                    "--board orangepi-5-plus-linux,orangepi-5-plus-starry",
                    commands,
                )

    def test_benchmark_suite_path_resolves_to_registered_axvisor_check(self) -> None:
        path = (
            "benchmarks/axvisor/board-orangepi-5-plus/vcpu-perf/"
            "performance/board-orangepi-5-plus-vcpu-perf.toml"
        )

        selections = ci_plan.resolve_suite_selections(
            ci_plan.WORKSPACE_ROOT,
            ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS),
            [path],
        )

        self.assertEqual(len(selections), 1)
        self.assertEqual(
            selections[0].template_id,
            "test-axvisor-self-hosted-board-orangepi-5-plus-vcpu-perf",
        )
        self.assertEqual(
            selections[0].command,
            "cargo xtask axvisor test board --test-group normal "
            "--test-case performance --board orangepi-5-plus-vcpu-perf",
        )

    def test_migrated_axvisor_suite_path_resolves_to_nightly_check(self) -> None:
        path = (
            "apps/axvisor/normal/qemu-timer-stress/gicv3-timer-stress/qemu-aarch64.toml"
        )

        selections = ci_plan.resolve_suite_selections(
            ci_plan.WORKSPACE_ROOT,
            ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS),
            [path],
        )

        self.assertEqual(len(selections), 1)
        self.assertEqual(
            selections[0].template_id,
            "test-axvisor-aarch64-qemu-timer-stress",
        )
        self.assertIn("--test-case gicv3-timer-stress", selections[0].command)

    def test_nightly_only_suite_changes_keep_static_checks_without_running_board(self):
        for path in (
            "apps/axvisor/normal/qemu-timer-stress/gicv3-timer-stress/qemu-aarch64.toml",
            # "benchmarks/axvisor/board-orangepi-5-plus/ivc-benchmark/benchmark/board-orangepi-5-plus-ivc-benchmark.toml",
            "apps/axvisor/normal/board-orangepi-5-plus/pci-network/ping/board-orangepi-5-plus-linux.toml",
            "apps/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/smoke/board-orangepi-5-plus-virtio-net-peer.toml",
            "benchmarks/axvisor/board-orangepi-5-plus/vcpu-perf/performance/board-orangepi-5-plus-vcpu-perf.toml",
            "benchmarks/axvisor/board-orangepi-5-plus/task-switch/board-orangepi-5-plus-task-switch.toml",
            "benchmarks/starry/block-rw-bench/board-orangepi-5-plus.toml",
            "benchmarks/starry/qemu/ltp-hackbench/qemu-x86_64-benchmark.toml",
        ):
            with self.subTest(path=path):
                context = ci_plan.replace(
                    self.upstream,
                    impact=ci_plan.CiImpact(
                        full=False, reason="fixture", changed_paths=(path,),
                        test_suite_paths=(path,), exclusive=True,
                    ),
                )
                plan = ci_plan.build_main_plan(context)
                self.assertTrue(plan["static_required"])
                self.assertFalse(main_test_rows(plan))
                self.assertFalse(plan["axvisor_required"])
                self.assertFalse(plan["starry_required"])

    def test_benchmark_starry_path_resolves_to_registered_benchmark_check(self):
        cases = {
            "benchmarks/starry/block-rw-bench/board-orangepi-5-plus.toml": (
                "starry-performance-block-rw-orangepi-5-plus",
                "-t benchmark/block-rw-bench",
            ),
            "benchmarks/starry/qemu/ltp-hackbench/qemu-x86_64-benchmark.toml": (
                "starry-performance-ltp-hackbench",
                "--qemu-config qemu-x86_64-benchmark.toml",
            ),
            "benchmarks/starry/orangepi-5-plus-uvc-rknn/configs/board-orangepi-5-plus-bench.toml": (
                "starry-performance-uvc-rknn-orangepi-5-plus",
                "--board-config configs/board-orangepi-5-plus-bench.toml",
            ),
        }
        for path, (template_id, fragment) in cases.items():
            with self.subTest(path=path):
                selections = ci_plan.resolve_suite_selections(
                    ci_plan.WORKSPACE_ROOT,
                    ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS),
                    [path],
                )
                self.assertEqual(len(selections), 1)
                self.assertEqual(selections[0].template_id, template_id)
                self.assertIn(fragment, selections[0].command)

    def test_functional_smoke_path_does_not_route_to_the_nightly_benchmark_check(
        self,
    ):
        path = "apps/starry/generated-app/config.toml"
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
                    for arch in ci_plan.ARCH_TARGETS
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

    def test_pure_test_suite_change_runs_only_the_exact_registered_case(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("test-suit/starryos/qemu/system/qemu-aarch64.toml",),
                test_suite_paths=("test-suit/starryos/qemu/system/qemu-aarch64.toml",),
                exclusive=True,
            ),
        )

        plan = ci_plan.build_main_plan(context)

        self.assertFalse(plan["static_required"])
        self.assertEqual(plan["static_matrix"]["include"], [])
        self.assertFalse(plan["workspace_required"])
        self.assertFalse(plan["arceos_required"])
        self.assertTrue(plan["starry_required"])
        self.assertFalse(plan["axvisor_required"])
        self.assertEqual(
            [row["name"] for row in plan["starry_matrix"]["include"]],
            ["QEMU aarch64 · qemu/system"],
        )
        self.assertEqual(
            plan["starry_matrix"]["include"][0]["command"],
            "cargo xtask starry test qemu --arch aarch64 --test-case qemu/system",
        )

    def test_pure_board_suite_change_runs_only_the_exact_board_case(self) -> None:
        path = (
            "test-suit/starryos/board-orangepi-5-plus/"
            "native-hardware-smoke/board-orangepi-5-plus.toml"
        )
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                test_suite_paths=(path,),
                exclusive=True,
            ),
        )

        plan = ci_plan.build_main_plan(context)

        self.assertFalse(plan["static_required"])
        self.assertEqual(
            [row["name"] for row in plan["axvisor_matrix"]["include"]],
            ["Board OrangePi 5 Plus · native-hardware-smoke"],
        )
        self.assertEqual(
            plan["axvisor_matrix"]["include"][0]["command"],
            "cargo xtask starry test board --test-case native-hardware-smoke "
            "--board orangepi-5-plus",
        )

    def test_starry_board_build_change_groups_cases_by_build_config(self) -> None:
        path = (
            "test-suit/starryos/board-orangepi-5-plus/"
            "build-aarch64-unknown-none-softfloat.toml"
        )
        selections = ci_plan.resolve_suite_selections(
            ci_plan.WORKSPACE_ROOT,
            ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS),
            [path],
        )

        self.assertEqual(len(selections), 1)
        self.assertEqual(
            selections[0].command,
            "cargo xtask starry test board --test-case "
            "exec-cache,native-hardware-smoke,native-network-smoke,pwm-sysfs,"
            "rknpu-resources --board orangepi-5-plus",
        )

    def test_sg2002_board_build_change_groups_all_feature_cases(self) -> None:
        path = (
            "test-suit/starryos/board-aka-00-sg2002/"
            "build-riscv64gc-unknown-none-elf.toml"
        )
        selections = ci_plan.resolve_suite_selections(
            ci_plan.WORKSPACE_ROOT,
            ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS),
            [path],
        )

        self.assertEqual(len(selections), 1)
        self.assertEqual(
            selections[0].command,
            "cargo xtask starry test board --test-case "
            "boot,tennis-yolo,usb2-lsusb,vdec,wifi-network-smoke "
            "--board aka-00-sg2002",
        )

    def test_starry_board_case_changes_share_one_incremental_build_row(self) -> None:
        paths = [
            "test-suit/starryos/board-orangepi-5-plus/exec-cache/"
            "board-orangepi-5-plus.toml",
            "test-suit/starryos/board-orangepi-5-plus/native-network-smoke/"
            "board-orangepi-5-plus.toml",
        ]
        selections = ci_plan.resolve_suite_selections(
            ci_plan.WORKSPACE_ROOT,
            ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS),
            paths,
        )

        self.assertEqual(len(selections), 1)
        self.assertEqual(
            selections[0].command,
            "cargo xtask starry test board --test-case exec-cache,native-network-smoke "
            "--board orangepi-5-plus",
        )

    def test_axvisor_board_build_change_groups_cases_by_build_config(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            suite_root = root / "test-suit/axvisor/normal/board-demo"
            (suite_root / "smoke").mkdir(parents=True)
            (suite_root / "ping").mkdir(parents=True)
            build_config = suite_root / "build-aarch64-unknown-none-softfloat.toml"
            build_config.write_text("target = 'aarch64-unknown-none-softfloat'\n")
            (suite_root / "smoke/board-demo.toml").write_text("\n")
            (suite_root / "ping/board-demo.toml").write_text("\n")
            checks = [
                {
                    "id": "axvisor-board-demo",
                    "name": "Board Demo · Suites",
                    "suite": [{"kind": "axvisor-board", "board": "demo"}],
                }
            ]

            selections = ci_plan.resolve_suite_selections(
                root,
                checks,
                [build_config.relative_to(root).as_posix()],
            )

        self.assertEqual(len(selections), 1)
        self.assertEqual(
            selections[0].command,
            "cargo xtask axvisor test board --test-group normal "
            "--test-case ping,smoke --board demo",
        )

    def test_generic_driver_suite_routes_source_and_rejects_missing_cases(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            case = root / "test-suit/arceos/drivers/packet-link"
            (case / "src").mkdir(parents=True)
            (case / "build-x86_64-unknown-none.toml").write_text("features = []\n")
            (case / "qemu-x86_64.toml").write_text("args = []\n")
            second_case = root / "test-suit/arceos/drivers/packet-datagram"
            (second_case / "src").mkdir(parents=True)
            (second_case / "build-x86_64-unknown-none.toml").write_text(
                "features = []\n"
            )
            (second_case / "qemu-x86_64.toml").write_text("args = []\n")
            source = "test-suit/arceos/drivers/packet-link/src/main.rs"
            (root / source).write_text("fn main() {}\n")
            second_source = "test-suit/arceos/drivers/packet-datagram/src/main.rs"
            (root / second_source).write_text("fn main() {}\n")
            registration = {"kind": "arceos-qemu", "arch": "x86_64", "group": "drivers"}
            checks = [{"id": "driver-suite", "name": "drivers", "suite": [registration]}]

            ci_plan._validate_suite_registrations([registration], "synthetic")
            with self.assertRaisesRegex(ci_plan.PlanError, "unsupported"):
                ci_plan._validate_suite_registrations(
                    [{**registration, "group": []}], "synthetic"
                )
            ci_plan.validate_suite_catalog(root, checks)
            selections = ci_plan.resolve_suite_selections(
                root, checks, [source, second_source]
            )
            self.assertEqual(
                {selection.source_path for selection in selections},
                {source, second_source},
            )
            self.assertEqual(len({selection.row_id for selection in selections}), 2)
            selection = next(
                selection for selection in selections if selection.source_path == source
            )
            self.assertEqual(selection.template_id, "driver-suite")
            self.assertEqual(
                selection.command,
                "cargo xtask arceos test qemu --arch x86_64 "
                "--test-group drivers --test-case packet-link",
            )
            registration["cases"] = ["missing-endpoint"]
            with self.assertRaisesRegex(ci_plan.SuiteRouteError, "missing"):
                ci_plan.validate_suite_catalog(root, checks)

    def test_cpu_vmx_suite_routes_to_the_registered_cpu_case(self) -> None:
        path = "test-suit/arceos/cpu/guest-entry/qemu-x86_64-vmx.toml"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                test_suite_paths=(path,),
                exclusive=True,
            ),
        )
        plan = ci_plan.build_main_plan(context)
        rows = plan["arceos_matrix"]["include"]
        self.assertEqual(len(rows), 1)
        self.assertIn("intel", rows[0]["runs_on"])
        self.assertEqual(
            rows[0]["command"],
            "cargo xtask arceos test qemu --arch x86_64 "
            "--test-group cpu --test-case guest-entry-vmx",
        )

    def test_board_suite_changes_share_one_build_configuration_row(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            wrapper = root / "test-suit/starryos/board-demo"
            (wrapper / "build-aarch64-unknown-none-softfloat.toml").parent.mkdir(
                parents=True
            )
            (wrapper / "build-aarch64-unknown-none-softfloat.toml").write_text(
                "features = []\n"
            )
            first = wrapper / "first/board-demo.toml"
            second = wrapper / "second/board-demo.toml"
            first.parent.mkdir()
            second.parent.mkdir()
            first.write_text("board = 'demo'\n")
            second.write_text("board = 'demo'\n")
            registration = {"kind": "starry-board", "board": "demo"}
            checks = [{"id": "board-suite", "name": "Board demo", "suite": [registration]}]
            ci_plan.validate_suite_catalog(root, checks)

            sources = [
                first.relative_to(root).as_posix(),
                second.relative_to(root).as_posix(),
            ]
            selections = ci_plan.resolve_suite_selections(root, checks, sources)

            self.assertEqual(len(selections), 1)
            self.assertEqual(selections[0].template_id, "board-suite")
            self.assertIn("first,second", selections[0].command)

    def test_cpu_pmu_board_routes_to_its_actual_case(self) -> None:
        path = "test-suit/arceos/board-orangepi-5-plus/pmu/board-orangepi-5-plus.toml"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                test_suite_paths=(path,),
                exclusive=True,
            ),
        )
        plan = ci_plan.build_main_plan(context)
        rows = plan["arceos_matrix"]["include"]
        self.assertEqual(len(rows), 1)
        self.assertIn("board", rows[0]["runs_on"])
        self.assertEqual(
            rows[0]["command"],
            "cargo xtask arceos test board --test-case pmu --board orangepi-5-plus",
        )

    def test_cpu_cpufreq_board_requires_actual_orangepi_case(self) -> None:
        path = (
            "test-suit/arceos/board-orangepi-5-plus/cpufreq/"
            "board-orangepi-5-plus.toml"
        )
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                test_suite_paths=(path,),
                exclusive=True,
            ),
        )
        plan = ci_plan.build_main_plan(context)
        rows = plan["arceos_matrix"]["include"]
        self.assertEqual(len(rows), 1)
        self.assertIn("board", rows[0]["runs_on"])
        self.assertEqual(
            rows[0]["command"],
            "cargo xtask arceos test board --test-case cpufreq "
            "--board orangepi-5-plus",
        )

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
                    "test-suit/starryos/generated-suite/config.toml",
                ),
                test_suite_paths=(
                    "test-suit/starryos/generated-suite/config.toml",
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
        path = "test-suit/starryos/qemu/generated-suite/config.toml"
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
                    for arch in ci_plan.ARCH_TARGETS
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
        self.assertTrue(any(row["group"] == "AxVisor" for row in rows.values()))
        self.assertEqual(plan["arceos_matrix"]["include"], [])

    def test_precise_board_input_does_not_select_same_os_qemu(self) -> None:
        catalog = ci_plan.load_catalog(ci_plan.MAIN_MANIFESTS)
        board_check = next(
            check
            for check in catalog
            if any(
                registration["kind"].endswith("-board")
                for registration in check.get("suite", ())
            )
        )
        board_registration = next(
            registration
            for registration in board_check["suite"]
            if registration["kind"].endswith("-board")
        )
        os_name = board_registration["kind"].partition("-")[0]
        board = board_registration["board"]
        qemu_ids = {
            check["id"]
            for check in catalog
            if any(
                registration["kind"] == f"{os_name}-qemu"
                for registration in check.get("suite", ())
            )
        }
        self.assertTrue(qemu_ids)

        context = ci_plan.replace(
            self.upstream,
            impact=ci_plan.CiImpact(
                full=False,
                reason="precise board input fixture",
                changed_paths=(),
                input_selections=(f"{os_name}:board:{board}",),
            ),
        )
        rows = self.assert_unique_ids(main_test_rows(ci_plan.build_main_plan(context)))

        self.assertIn(board_check["id"], rows)
        self.assertTrue(qemu_ids.isdisjoint(rows))

    def test_dualguest_robot_board_is_not_scheduled(self) -> None:
        rows = self.assert_unique_ids(
            ci_plan.build_main_plan(self.upstream)["axvisor_matrix"]["include"]
        )
        self.assertNotIn("test-orangepi-5-plus-dualguest-robot", rows)
        nightly_rows = self.assert_unique_ids(
            ci_plan.build_axvisor_nightly_plan(
                ci_plan.replace(self.upstream, event_name="schedule")
            )["axvisor_matrix"]["include"]
        )
        self.assertNotIn("test-orangepi-5-plus-dualguest-robot", nightly_rows)
        self.assertNotIn(
            "test-axvisor-self-hosted-board-orangepi-5-plus-ivc-benchmark",
            nightly_rows,
        )
        benchmark_rows = self.assert_unique_ids(
            ci_plan.build_benchmarks_plan(
                ci_plan.replace(self.upstream, event_name="schedule")
            )["axvisor_performance_matrix"]["include"]
        )
        self.assertNotIn("test-orangepi-5-plus-dualguest-robot", benchmark_rows)
        self.assertNotIn(
            "test-axvisor-self-hosted-board-orangepi-5-plus-ivc-benchmark",
            benchmark_rows,
        )

    def test_dualguest_robot_board_markers_cannot_match_command_echo(self) -> None:
        root = MODULE_PATH.parents[2]
        configs = (
            root
            / "test-suit/axvisor/normal/board-orangepi-5-plus/dual-linux-zephyr"
            / "board-orangepi-5-plus-dualguest-robot.toml",
            root
            / "test-suit/axvisor/normal/board-orangepi-5-plus/dual-starry-zephyr"
            / "board-orangepi-5-plus-dualguest-robot.toml",
        )

        for path in configs:
            with self.subTest(config=path):
                config = tomllib.loads(path.read_text())
                self.assertEqual(
                    config["board_type"], "OrangePi-5-Plus-DualGuest-robot"
                )
                step = config["shell_check_steps"][-1]
                for pattern in step["success_regex"] + step["fail_regex"]:
                    self.assertIsNone(re.search(pattern, step["shell_cmd"]))
                guest = "linux-zephyr" if "dual-linux" in str(path) else "starry-zephyr"
                self.assertTrue(any(re.search(pattern, f"DUAL_PICK_CI_PASS guest={guest}\n")
                                    for pattern in step["success_regex"]))
                self.assertTrue(any(re.search(pattern, f"DUAL_PICK_CI_FAIL guest={guest} status=1\n")
                                    for pattern in step["fail_regex"]))

    def test_ivc_benchmark_board_runs_benchmark_from_guest_shell(self) -> None:
        root = MODULE_PATH.parents[2]
        case_dir = (
            root / "benchmarks/axvisor/board-orangepi-5-plus/ivc-benchmark"
        )
        vm_config = tomllib.loads(
            (case_dir / "starry-axivc-benchmark.toml").read_text()
        )
        board_config = tomllib.loads(
            (
                case_dir / "benchmark/board-orangepi-5-plus-ivc-benchmark.toml"
            ).read_text()
        )

        benchmark = "/usr/bin/ivc-starry-bench"
        cmdline = vm_config["kernel"]["cmdline"]
        # The board route waits for the default Starry init shell prompt, and
        # StarryOS panics when init exits, so the VM must keep that shell as
        # init and run the benchmark as its child.
        self.assertNotIn(benchmark, cmdline)
        self.assertNotIn("init=", cmdline)

        steps = board_config["shell_check_steps"]
        attach_indices = [
            index
            for index, step in enumerate(steps)
            if step.get("shell_cmd", "").strip() == "vm console 1"
        ]
        launch_indices = [
            index
            for index, step in enumerate(steps)
            if benchmark in step.get("shell_cmd", "")
        ]
        self.assertEqual(len(attach_indices), 1)
        self.assertEqual(len(launch_indices), 1)
        self.assertLess(attach_indices[0], launch_indices[0])

        launch = steps[launch_indices[0]]
        self.assertEqual(launch["shell_prefix"], "root@starry:")
        pass_pattern = (
            "(?m)^(?:\\[VM 1\\] )?AXVISOR_IVC_BENCH_RESULT=PASS "
            "cases=4 testTime=100 bytes=1232076800 chunks=400\\s*$"
        )
        self.assertEqual(launch["success_regex"], [pass_pattern])
        for pattern in launch["success_regex"]:
            self.assertIsNone(re.search(pattern, launch["shell_cmd"]))
        marker = (
            "AXVISOR_IVC_BENCH_RESULT=PASS cases=4 testTime=100 "
            "bytes=1232076800 chunks=400"
        )
        self.assertTrue(
            any(
                re.search(pattern, f"{marker}\n")
                for pattern in launch["success_regex"]
            )
        )
        self.assertTrue(
            any(
                re.search(pattern, f"[VM 1] {marker}\n")
                for pattern in launch["success_regex"]
            )
        )
        mismatch = marker.replace("bytes=1232076800", "bytes=1232076799")
        self.assertIsNone(re.search(pass_pattern, f"{mismatch}\n"))

        fail_patterns = board_config["fail_regex"]
        for sample in (
            "Kernel panic - not syncing\n",
            "panicked at kernel/src/task/exit.rs: Attempted to kill init!\n",
            "AXIVC Starry benchmark peer ready failed\n",
            "AXIVC Starry benchmark send failed\n",
            "AXIVC Starry benchmark recv failed\n",
            "AXIVC Zephyr-Starry benchmark failed\n",
        ):
            with self.subTest(sample=sample):
                self.assertTrue(
                    any(re.search(pattern, sample) for pattern in fail_patterns)
                )

    def test_single_client_robot_check_routing_and_real_board_contract(self) -> None:
        root = MODULE_PATH.parents[2]
        real_starry = (
            root
            / "test-suit/starryos/board-orangepi-5-plus/robot-flow"
            / "board-orangepi-5-plus-robot-real.toml"
        )
        real_axvisor_starry = (
            root
            / "apps/axvisor/normal/board-orangepi-5-plus/robot-real-starry/smoke"
            / "board-orangepi-5-plus-robot-real-starry.toml"
        )
        real_axvisor_linux = (
            root
            / "apps/axvisor/normal/board-orangepi-5-plus/robot-real-linux/smoke"
            / "board-orangepi-5-plus-robot-real-linux.toml"
        )
        real_starry_guest = (
            root
            / "apps/axvisor/normal/board-orangepi-5-plus/robot-real-starry/guest.toml"
        )
        real_linux_guest = (
            root
            / "apps/axvisor/normal/board-orangepi-5-plus/robot-real-linux"
            / "linux-smp1-emmc.toml"
        )

        main = ci_plan.build_main_plan(self.upstream)
        starry_rows = self.assert_unique_ids(main["starry_matrix"]["include"])
        axvisor_rows = self.assert_unique_ids(main["axvisor_matrix"]["include"])
        nightly_rows = self.assert_unique_ids(
            ci_plan.build_axvisor_nightly_plan(
                ci_plan.replace(self.upstream, event_name="schedule")
            )["axvisor_matrix"]["include"]
        )

        self.assertNotIn("test-orangepi-5-plus-robot-real-native-starryos", starry_rows)
        self.assertNotIn("test-orangepi-5-plus-robot-real-suite", axvisor_rows)
        real_suite_id = "test-orangepi-5-plus-robot-real-suite"
        self.assertIn(real_suite_id, nightly_rows)
        real_suite_command = nightly_rows[real_suite_id]["command"]
        self.assertIn("cargo xtask starry test board --board orangepi-5-plus-robot-real", real_suite_command)
        self.assertIn(
            "--board orangepi-5-plus-robot-real-starry,orangepi-5-plus-robot-real-linux",
            real_suite_command,
        )

        catalog = {
            check["id"]: check
            for check in ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS)
        }
        robot_check = catalog["test-orangepi-5-plus-robot-axvisor-guests"]
        registered_guest_boards = {
            suite["board"]
            for suite in robot_check["suite"]
            if suite.get("kind") == "axvisor-board"
        }
        self.assertEqual(
            registered_guest_boards,
            {"orangepi-5-plus-robot-starry", "orangepi-5-plus-robot-linux"},
        )

        for path in (real_starry, real_axvisor_starry, real_axvisor_linux):
            with self.subTest(config=path):
                config = tomllib.loads(path.read_text())
                self.assertEqual(config["board_type"], "OrangePi-5-Plus-robot")
                self.assertNotIn("uboot_cmd", config)
                commands = "\n".join(
                    step["shell_cmd"] for step in config["shell_check_steps"]
                )
                self.assertIn("FEETECH_DEV=auto", commands)
                self.assertIn("./run_robot_ci_once.sh 28.0", commands)
                self.assertNotIn("/dev/ttyS6", commands)
                if path == real_axvisor_linux:
                    self.assertIn("sudo -S env FEETECH_DEV=auto", commands)

        for path in (real_starry_guest, real_linux_guest):
            text = path.read_text()
            self.assertNotIn("include_default_passthrough", text)
            self.assertNotIn("/serial@feb90000", text)

        starry_kernel = tomllib.loads(real_starry_guest.read_text())["kernel"]
        self.assertEqual(
            starry_kernel["kernel_path"],
            "${workspace}/target/aarch64-unknown-none-softfloat/release/starryos.bin",
        )
        linux_kernel = tomllib.loads(real_linux_guest.read_text())["kernel"]
        self.assertEqual(
            linux_kernel["kernel_path"], "/guest/linux/orangepi-5-plus-6.1.99"
        )
        self.assertIn("root=/dev/mmcblk1p2", linux_kernel["cmdline"])

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
        self.assertTrue(
            any(row["runs_on"] == ["ubuntu-latest"] for row in rows.values())
        )

        workspace_root = MODULE_PATH.parents[2]
        catalog = ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS)
        board_path = None
        for candidate in sorted(
            (workspace_root / "test-suit").glob("**/board-*/*/*.toml")
        ):
            relative = candidate.relative_to(workspace_root).as_posix()
            try:
                ci_plan.resolve_suite_selections(workspace_root, catalog, [relative])
            except ci_plan.SuiteRouteError:
                continue
            board_path = relative
            break
        self.assertIsNotNone(board_path)
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
        declared_groups = {
            check["resource_group"]
            for check in catalog.values()
            if check.get("resource_group")
        }
        board_groups: dict[str, set[str]] = {}
        for row in (*main_rows, *nightly_rows, *benchmark_rows):
            check = catalog[row["id"]]
            boards = {
                registration["board"]
                for registration in check.get("suite", ())
                if "board" in registration
            }
            if boards:
                self.assertIn("board", row["runs_on"])
                for board in boards:
                    board_groups.setdefault(board, set()).add(
                        row["resource_group"]
                    )
                possible_groups = set.intersection(
                    *(
                        {
                            group
                            for group in declared_groups
                            if board == group or board.startswith(f"{group}-")
                        }
                        for board in boards
                    )
                )
                if possible_groups:
                    self.assertTrue(
                        row["resource_group"],
                        f"board checks must declare a resource group: {boards}",
                    )
                    self.assertIn(row["resource_group"], possible_groups)
                else:
                    self.assertEqual(row["resource_group"], "")
                self.assertEqual(
                    row["resource_group"], check.get("resource_group", "")
                )
            else:
                self.assertNotIn("board", row["runs_on"])
                self.assertEqual(row["resource_group"], "")

        for board, groups in board_groups.items():
            self.assertLessEqual(
                len(groups),
                1,
                f"one registered board must use one resource group: {board}",
            )

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

    def test_arceos_ivc_app_changes_select_their_qemu_cases(self) -> None:
        scenarios = (
            ("apps/arceos/ivc_publisher/src/main.rs", "--test-case qemu-ivc-local"),
            ("apps/arceos/ivc_subscriber/src/main.rs", "--test-case qemu-ivc-arceos"),
        )
        for path, command in scenarios:
            with self.subTest(path=path):
                impact = ci_impact.analyze_changed_paths(
                    ci_plan.WORKSPACE_ROOT, [Path(path)], {}
                )
                self.assertFalse(impact.full)
                self.assertEqual(impact.input_selections, ("axvisor:qemu:aarch64",))
                self.assertNotIn(path, impact.ignored_apps)
                rows = ci_plan.build_main_plan(
                    ci_plan.replace(self.upstream, impact=impact)
                )["axvisor_matrix"]["include"]
                check = next(
                    row
                    for row in rows
                    if row["id"]
                    == "test-axvisor-aarch64-qemu-http-control-plane-browser-console-ivc"
                )
                self.assertIn(command, check["command"])

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
