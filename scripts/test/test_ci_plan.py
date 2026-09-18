#!/usr/bin/env python3

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

MODULE_PATH = Path(__file__).with_name("ci_plan.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("ci_plan", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ci_plan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_plan)
PERF_REPORT_SPEC = importlib.util.spec_from_file_location(
    "ci_perf_report", MODULE_PATH.with_name("ci_perf_report.py")
)
assert PERF_REPORT_SPEC is not None and PERF_REPORT_SPEC.loader is not None
ci_perf_report = importlib.util.module_from_spec(PERF_REPORT_SPEC)
PERF_REPORT_SPEC.loader.exec_module(ci_perf_report)

MAIN_TEST_PREFIXES = ("workspace", "arceos", "starry", "axvisor")
MAIN_TEST_GROUPS = ("Workspace", "ArceOS", "Starry", "AxVisor")


def main_test_rows(plan: dict) -> list[dict]:
    return [
        row
        for prefix in MAIN_TEST_PREFIXES
        for row in plan[f"{prefix}_matrix"]["include"]
    ]


class CiPlanTests(unittest.TestCase):
    def test_axvisor_nightly_runs_all_registered_checks_with_artifact_producer(self):
        catalog = ci_plan.load_catalog(ci_plan.MAIN_MANIFESTS)
        expected = {check["id"] for check in catalog if check["group"] == "AxVisor"}
        for event in ("schedule", "workflow_dispatch"):
            with self.subTest(event=event):
                context = ci_plan.PlanContext(
                    repository="rcore-os/tgoskits",
                    repository_owner="rcore-os",
                    event_name=event,
                )
                plan = ci_plan.build_axvisor_nightly_plan(context)
                rows = plan["axvisor_matrix"]["include"]
                self.assertEqual({row["id"] for row in rows}, expected)
                self.assertEqual(len(rows), len(expected))
                self.assertTrue(any(
                    "--board orangepi-5-plus-linux --test-case ping" in row["command"]
                    for row in rows
                ))
                producer, = plan["prepare_matrix"]["include"]
                self.assertTrue(producer["upload_xtask_bin_artifact"])
                self.assertEqual(producer["command"], "cargo build -p tg-xtask")
                for row in rows:
                    if row["download_xtask_bin_artifact"]:
                        self.assertEqual(
                            row["xtask_bin_artifact_name"], producer["xtask_bin_artifact_name"]
                        )
                performance_rows = {
                    row["id"]
                    for row in rows
                    if row["performance_report"]
                }
                self.assertEqual(
                    performance_rows,
                    {
                        "test-axvisor-self-hosted-board-orangepi-5-plus-ivc-benchmark",
                        "test-axvisor-self-hosted-board-orangepi-5-plus-vcpu-perf",
                    },
                )
                main = ci_plan.build_main_plan(context)
                nightly_ids = {
                    check["id"] for check in catalog if check.get("nightly_only", False)
                }
                self.assertEqual(
                    [row for row in rows if row["id"] not in nightly_ids],
                    main["axvisor_matrix"]["include"],
                )

    def test_main_ci_never_runs_axvisor_nightly_only_cases(self):
        for event in ("pull_request", "push", "workflow_dispatch", "schedule"):
            with self.subTest(event=event):
                context = ci_plan.replace(self.upstream, event_name=event)
                rows = ci_plan.build_main_plan(context)["axvisor_matrix"]["include"]
                commands = "\n".join(row["command"] for row in rows)
                self.assertNotIn("timer-stress", commands)
                self.assertNotIn("ivc-benchmark", commands)
                self.assertNotIn("orangepi-5-plus-vcpu-perf", commands)
                self.assertNotIn("--test-case ping", commands)
                self.assertIn("--board orangepi-5-plus-linux --test-case smoke", commands)
                self.assertNotIn("--board orangepi-5-plus-linux\n", commands)
                self.assertIn("--test-case qemu-ivc", commands)
                self.assertIn("--board orangepi-5-plus-starry", commands)

    def test_nightly_only_suite_changes_keep_static_checks_without_running_board(self):
        for path in (
            "test-suit/axvisor/normal/qemu-timer-stress/gicv3-timer-stress/qemu-aarch64.toml",
            "test-suit/axvisor/normal/board-orangepi-5-plus/ivc-benchmark/benchmark/board-orangepi-5-plus-ivc-benchmark.toml",
            "test-suit/axvisor/normal/board-orangepi-5-plus/pci-network/ping/board-orangepi-5-plus-linux.toml",
            "test-suit/axvisor/normal/board-orangepi-5-plus/vcpu-perf/performance/board-orangepi-5-plus-vcpu-perf.toml",
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

    def test_axvisor_nightly_rejects_incremental_pr_mode(self):
        with self.assertRaises(ci_plan.PlanError):
            ci_plan.build_axvisor_nightly_plan(self.upstream)

    def test_performance_report_renders_supported_axvisor_results(self):
        report = ci_perf_report.render_report(
            "test-axvisor-self-hosted-board-orangepi-5-plus-vcpu-perf",
            "Board OrangePi 5 Plus · Single ArceOS guest performance",
            "\n".join(
                [
                    "[VM 1] VCPU_PERF_SAMPLE index=0 blocks=1099483 "
                    "elapsed_ns=3000001123 timer_wakes=3011 checksum=841832",
                    "[VM 1] VCPU_PERF_SAMPLE index=1 blocks=1100123 "
                    "elapsed_ns=3000000987 timer_wakes=3010 checksum=841890",
                    "[VM 1] VCPU_PERF_RESULT blocks_per_second=365334.20 "
                    "baseline=364822.00 threshold=328339.80 samples=[364474.50,365334.20]",
                    "[VM 1] VCPU_PERF_PASS",
                ]
            ),
        )

        self.assertIn("#### vCPU samples (per window)", report)
        self.assertIn(
            "| index | blocks | elapsed_ns | timer_wakes | checksum |", report
        )
        self.assertIn("| 1 | 1100123 | 3000000987 | 3010 | 841890 |", report)
        self.assertIn("#### vCPU throughput result", report)
        self.assertIn(
            "| blocks_per_second | baseline | threshold | samples |", report
        )
        self.assertIn(
            "| 365334.20 | 364822.00 | 328339.80 | [364474.50,365334.20] |", report
        )

    def test_performance_report_renders_axivc_benchmark_result(self):
        report = ci_perf_report.render_report(
            "test-axvisor-self-hosted-board-orangepi-5-plus-ivc-benchmark",
            "Board OrangePi 5 Plus · AXIVC Zephyr-Starry benchmark",
            "\n".join(
                [
                    "[test_output] ========================================",
                    "[test_output] average sendBandwidth = 2263.10 MB/s, "
                    "average receiveBandwidth = 1505.03 MB/s, "
                    "testTime = 100, datasize = 262144",
                    "[test_output] average sendBandwidth = 2287.42 MB/s, "
                    "average receiveBandwidth = 2045.21 MB/s, "
                    "testTime = 100, datasize = 1048576",
                    "AXVISOR_IVC_BENCH_RESULT=PASS cases=4 testTime=100 "
                    "bytes=1232076800 chunks=400",
                ]
            ),
        )

        self.assertIn("#### AXIVC benchmark per-case bandwidth", report)
        self.assertIn(
            "| datasize | sendBandwidth (MB/s) | receiveBandwidth (MB/s) | testTime |",
            report,
        )
        self.assertIn("| 262144 (256 KiB) | 2263.10 | 1505.03 | 100 |", report)
        self.assertIn("| 1048576 (1 MiB) | 2287.42 | 2045.21 | 100 |", report)
        self.assertIn("#### AXIVC benchmark result", report)
        self.assertIn("| status | cases | testTime | bytes | chunks |", report)
        self.assertIn("| PASS | 4 | 100 | 1232076800 | 400 |", report)

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

        self.assertIn("test-axloader-http-smoke", ids)
        self.assertFalse(any(check_id.startswith("test-arceos-") for check_id in ids))
        self.assertFalse(any(check_id.startswith("test-starry-") for check_id in ids))

    def test_incremental_pr_uses_std_since_but_full_pr_does_not(self) -> None:
        incremental = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
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
            impact=ci_plan.CiImpact.full_selection("fixture"),
        )

        incremental_rows = self.assert_unique_ids(
            main_test_rows(ci_plan.build_main_plan(incremental))
        )
        full_rows = self.assert_unique_ids(
            main_test_rows(ci_plan.build_main_plan(full))
        )

        self.assertIn(
            '--since "$SINCE_REF"', incremental_rows["test-with-std"]["command"]
        )
        self.assertNotIn("--since", full_rows["test-with-std"]["command"])

    def test_app_only_impact_does_not_select_runtime_checks(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
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
            [row["name"] for row in plan["starry_matrix"]["include"]],
            ["Board OrangePi 5 Plus · native-hardware-smoke"],
        )
        self.assertEqual(
            plan["starry_matrix"]["include"][0]["command"],
            "cargo xtask starry test board --test-case native-hardware-smoke "
            "--board orangepi-5-plus",
        )

    def test_cpu_vmx_suite_routes_to_the_registered_cpu_case(self) -> None:
        path = "test-suit/arceos/cpu/guest-entry/qemu-x86_64-vmx.toml"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
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

    def test_cpu_pmu_board_routes_to_its_actual_case(self) -> None:
        path = "test-suit/arceos/board-orangepi-5-plus/pmu/board-orangepi-5-plus.toml"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
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

    def test_unregistered_test_suite_fails_planning(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(
                    "test-suit/starryos/board-rock-4d/boot/board-rock-4d.toml",
                ),
                test_suite_paths=(
                    "test-suit/starryos/board-rock-4d/boot/board-rock-4d.toml",
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

    def test_multiple_test_suite_changes_form_a_stable_exact_union(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(
                    "test-suit/axvisor/normal/qemu-acpi/direct-acpi/qemu-x86_64-vmx.toml",
                    "test-suit/starryos/qemu/system/qemu-aarch64.toml",
                ),
                test_suite_paths=(
                    "test-suit/axvisor/normal/qemu-acpi/direct-acpi/qemu-x86_64-vmx.toml",
                    "test-suit/starryos/qemu/system/qemu-aarch64.toml",
                ),
                exclusive=True,
            ),
        )

        plan = ci_plan.build_main_plan(context)
        axvisor_rows = plan["axvisor_matrix"]["include"]
        starry_rows = plan["starry_matrix"]["include"]

        self.assertEqual(
            [row["name"] for row in axvisor_rows],
            ["VMX x86_64 · direct-acpi-vmx"],
        )
        self.assertEqual(
            [row["name"] for row in starry_rows],
            ["QEMU aarch64 · qemu/system"],
        )
        self.assertTrue(
            all(
                not row["download_xtask_bin_artifact"]
                for row in axvisor_rows + starry_rows
            )
        )

    def test_starry_grouped_subcase_runs_only_that_subcase_on_registered_arches(
        self,
    ) -> None:
        path = "test-suit/starryos/qemu/system/test-pivot-root/src/main.c"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                test_suite_paths=(path,),
                exclusive=True,
            ),
        )

        rows = ci_plan.build_main_plan(context)["starry_matrix"]["include"]

        self.assertEqual(len(rows), 4)
        self.assertEqual(
            {row["name"] for row in rows},
            {
                f"QEMU {arch} · qemu/test-pivot-root"
                for arch in ("aarch64", "loongarch64", "riscv64", "x86_64")
            },
        )
        self.assertTrue(
            all("--test-case qemu/test-pivot-root" in row["command"] for row in rows)
        )

    def test_precise_board_input_does_not_select_same_arch_qemu(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("os/StarryOS/configs/board/visionfive2.toml",),
                input_selections=("starry:board:visionfive2",),
                targets=("starry:riscv64",),
            ),
        )

        ids = {
            row["id"]
            for row in main_test_rows(ci_plan.build_main_plan(context))
        }

        self.assertIn("test-starry-self-hosted-board-visionfive2", ids)
        self.assertNotIn("test-starry-riscv64-qemu", ids)

    def test_suite_plus_os_wide_crate_uses_the_broader_os_checks(self) -> None:
        path = "test-suit/starryos/qemu/system/qemu-aarch64.toml"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
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

        self.assertTrue(
            {
                "run-clippy",
                "test-with-std",
                "test-arceos-aarch64-qemu-app-suites",
                "test-axvisor-aarch64-qemu-http-control-plane-browser-console-ivc",
                "test-starry-aarch64-qemu",
            }.issubset(test_rows)
        )
        self.assertFalse(any("board" in row["id"] for row in test_rows.values()))
        self.assertTrue(
            all(
                row["runs_on"] == ["ubuntu-latest"]
                for row in (*static_rows.values(), *test_rows.values())
            )
        )
        self.assertEqual(static_rows["check-formatting"]["runs_on"], ["ubuntu-latest"])
        self.assertEqual(
            static_rows["check-formatting"]["container_image"],
            "ghcr.io/contributor/tgoskits-container:latest",
        )
        self.assertFalse(static_rows["check-formatting"]["download_xtask_bin_artifact"])
        clippy = test_rows["run-clippy"]
        self.assertEqual(clippy["runs_on"], ["ubuntu-latest"])
        self.assertEqual(clippy["fetch_depth"], "100")
        self.assertTrue(clippy["download_xtask_bin_artifact"])

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
