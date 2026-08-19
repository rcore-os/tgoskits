#!/usr/bin/env python3

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import tomllib

MODULE_PATH = Path(__file__).with_name("ci_plan.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("ci_plan", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ci_plan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_plan)


class CiPlanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.upstream = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            base_ref="dev",
        )

    def test_upstream_main_plan_preserves_all_checks(self) -> None:
        plan = ci_plan.build_main_plan(self.upstream)

        self.assertEqual(len(plan["static_matrix"]["include"]), 3)
        self.assertEqual(len(plan["test_matrix"]["include"]), 32)
        self.assertTrue(plan["static_required"])
        self.assertTrue(
            all(" / " in row["name"] for row in plan["test_matrix"]["include"])
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
        ids = {row["id"] for row in plan["test_matrix"]["include"]}

        self.assertEqual(
            ids,
            {
                "run-clippy",
                "test-with-std",
                "test-arceos-x86-64-qemu",
                "test-arceos-riscv64-qemu",
                "test-arceos-aarch64-qemu-app-suites",
                "test-arceos-loongarch64-qemu",
            },
        )

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
        ids = {row["id"] for row in plan["test_matrix"]["include"]}

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

        incremental_rows = {
            row["id"]: row
            for row in ci_plan.build_main_plan(incremental)["test_matrix"]["include"]
        }
        full_rows = {
            row["id"]: row
            for row in ci_plan.build_main_plan(full)["test_matrix"]["include"]
        }

        self.assertIn(
            '--since "$SINCE_REF"', incremental_rows["test-with-std"]["command"]
        )
        self.assertNotIn("--since", full_rows["test-with-std"]["command"])
        self.assertEqual(len(full_rows), 32)

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
        test_ids = {row["id"] for row in plan["test_matrix"]["include"]}

        self.assertEqual(test_ids, {"run-clippy", "test-with-std"})

    def test_non_pr_events_ignore_impact_and_preserve_full_matrix(self) -> None:
        impact = ci_plan.CiImpact(
            full=False,
            reason="must be ignored outside pull requests",
            changed_paths=("virtualization/arm_vcpu/src/lib.rs",),
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
                self.assertEqual(len(with_impact["test_matrix"]["include"]), 32)

    def test_display_names_expose_target_and_purpose(self) -> None:
        plan = ci_plan.build_main_plan(self.upstream)
        static_rows = {
            row["id"]: row["name"] for row in plan["static_matrix"]["include"]
        }
        test_rows = {row["id"]: row["name"] for row in plan["test_matrix"]["include"]}

        self.assertEqual(
            static_rows["check-formatting"], "Formatting + publish dry-run"
        )
        self.assertEqual(test_rows["run-clippy"], "Workspace / Incremental Clippy")
        self.assertEqual(
            test_rows["test-arceos-aarch64-qemu-app-suites"],
            "ArceOS / QEMU aarch64 · GICv2 SMP4 boot + suites + axtest",
        )
        self.assertEqual(
            test_rows["test-axvisor-aarch64-qemu-panic-modes"],
            "AxVisor / QEMU aarch64 · Panic modes",
        )
        self.assertEqual(
            test_rows["test-starry-self-hosted-board-visionfive2"],
            "Starry / Board VisionFive 2 · Suites",
        )

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
        self.assertEqual(
            [row["name"] for row in plan["test_matrix"]["include"]],
            ["Starry / QEMU aarch64 · qemu/system"],
        )
        self.assertEqual(
            plan["test_matrix"]["include"][0]["command"],
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
            [row["name"] for row in plan["test_matrix"]["include"]],
            ["Starry / Board OrangePi 5 Plus · native-hardware-smoke"],
        )
        self.assertEqual(
            plan["test_matrix"]["include"][0]["command"],
            "cargo xtask starry test board --test-case native-hardware-smoke "
            "--board orangepi-5-plus",
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

        rows = ci_plan.build_main_plan(context)["test_matrix"]["include"]

        self.assertEqual(
            [row["name"] for row in rows],
            [
                "AxVisor / VMX x86_64 · direct-acpi-vmx",
                "Starry / QEMU aarch64 · qemu/system",
            ],
        )
        self.assertTrue(all(not row["download_xtask_bin_artifact"] for row in rows))

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

        rows = ci_plan.build_main_plan(context)["test_matrix"]["include"]

        self.assertEqual(len(rows), 4)
        self.assertEqual(
            {row["name"] for row in rows},
            {
                f"Starry / QEMU {arch} · qemu/test-pivot-root"
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
            for row in ci_plan.build_main_plan(context)["test_matrix"]["include"]
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
        ids = {row["id"] for row in plan["test_matrix"]["include"]}

        self.assertTrue(plan["static_required"])
        self.assertEqual(
            len(
                [
                    row
                    for row in plan["test_matrix"]["include"]
                    if row["group"] == "Starry"
                ]
            ),
            9,
        )
        self.assertFalse(any(check_id.startswith("suite-") for check_id in ids))

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

        rows = ci_plan.build_main_plan(context)["test_matrix"]["include"]

        self.assertEqual(
            len([row for row in rows if row["group"] == "Starry"]),
            9,
        )
        self.assertFalse(any(row["group"] in {"ArceOS", "AxVisor"} for row in rows))

    def test_schema_v2_manifest_is_rejected_without_compatibility(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            manifest = Path(temp_dir) / "legacy.toml"
            manifest.write_text(
                """
schema_version = 2
phase = "test"
group = "Legacy"

[[check]]
id = "legacy-check"
name = "Legacy"
runs_on = ["ubuntu-latest"]
environment = "base"
command = "true"
""",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(ci_plan.PlanError, "schema_version = 3"):
                ci_plan._load_manifest(manifest)

    def test_arceos_qemu_jobs_run_same_arch_axtests_serially(self) -> None:
        plan = ci_plan.build_main_plan(self.upstream)
        rows = {row["id"]: row for row in plan["test_matrix"]["include"]}
        expected_arches = {
            "test-arceos-x86-64-qemu": "x86_64",
            "test-arceos-riscv64-qemu": "riscv64",
            "test-arceos-aarch64-qemu-app-suites": "aarch64",
            "test-arceos-loongarch64-qemu": "loongarch64",
        }

        for check_id, arch in expected_arches.items():
            command = rows[check_id]["command"]
            arceos_command = f"cargo xtask arceos test qemu --arch {arch}"
            axtest_command = (
                "cargo xtask ktest qemu --workspace --exclude starry-kernel "
                f"--exclude axvisor --arch {arch}"
            )
            self.assertIn(arceos_command, command)
            self.assertIn(axtest_command, command)
            self.assertLess(
                command.index(arceos_command), command.index(axtest_command)
            )
            self.assertEqual(rows[check_id]["cache_key"], "")

    def test_fork_filters_owner_checks_and_falls_back_from_qcs(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="contributor",
            event_name="pull_request",
            base_ref="dev",
        )
        plan = ci_plan.build_main_plan(context)
        static_rows = {row["id"]: row for row in plan["static_matrix"]["include"]}
        test_rows = {row["id"]: row for row in plan["test_matrix"]["include"]}

        self.assertEqual(len(test_rows), 16)
        self.assertFalse(any("board" in row["id"] for row in test_rows.values()))
        self.assertEqual(static_rows["check-formatting"]["runs_on"], ["ubuntu-latest"])
        self.assertEqual(
            static_rows["check-formatting"]["container_image"],
            "ghcr.io/rcore-os/tgoskits-container:latest",
        )
        self.assertFalse(static_rows["check-formatting"]["download_xtask_bin_artifact"])
        clippy = test_rows["run-clippy"]
        self.assertEqual(clippy["runs_on"], ["ubuntu-latest"])
        self.assertEqual(clippy["fetch_depth"], "0")
        self.assertTrue(clippy["download_xtask_bin_artifact"])

    def test_upstream_full_history_uses_bounded_self_hosted_checkout(self) -> None:
        plan = ci_plan.build_main_plan(self.upstream)
        rows = {row["id"]: row for row in plan["test_matrix"]["include"]}

        self.assertEqual(rows["run-clippy"]["fetch_depth"], "2")

    def test_default_values_are_materialized(self) -> None:
        plan = ci_plan.build_main_plan(self.upstream)
        rows = {row["id"]: row for row in plan["test_matrix"]["include"]}
        defaults = rows["test-with-std"]

        self.assertEqual(defaults["cache_key"], "")
        self.assertEqual(defaults["apk_region"], "china")
        self.assertEqual(defaults["fetch_depth"], "1")
        self.assertEqual(defaults["timeout_minutes"], 360)
        self.assertFalse(defaults["require_kvm"])
        self.assertFalse(defaults["upload_xtask_bin_artifact"])
        self.assertFalse(defaults["download_xtask_bin_artifact"])

    def test_base_event_and_boolean_input_selection(self) -> None:
        base_check = {"required_base_branch": "dev"}
        event_or_input_check = {
            "events": ["schedule"],
            "enable_boolean_input": "run_clippy_all",
        }
        pull_request_dev = self.upstream
        pull_request_main = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            base_ref="main",
        )
        scheduled = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="schedule",
        )
        manual = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="workflow_dispatch",
        )
        manual_enabled = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="workflow_dispatch",
            enabled_boolean_inputs=frozenset({"run_clippy_all"}),
        )

        self.assertTrue(ci_plan._is_enabled(base_check, pull_request_dev))
        self.assertFalse(ci_plan._is_enabled(base_check, pull_request_main))
        self.assertFalse(ci_plan._is_enabled(base_check, scheduled))
        self.assertTrue(ci_plan._is_enabled(event_or_input_check, scheduled))
        self.assertFalse(ci_plan._is_enabled(event_or_input_check, manual))
        self.assertTrue(ci_plan._is_enabled(event_or_input_check, manual_enabled))

    def test_incremental_commands_use_since_ref_environment(self) -> None:
        plan = ci_plan.build_main_plan(self.upstream)
        rows = plan["static_matrix"]["include"] + plan["test_matrix"]["include"]
        commands = {row["id"]: row["command"] for row in rows}

        self.assertIn('"$SINCE_REF"', commands["run-sync-lint"])
        self.assertIn('"$SINCE_REF"', commands["run-clippy"])
        self.assertNotIn("${{", commands["run-sync-lint"])
        self.assertNotIn("${{", commands["run-clippy"])

    def test_starry_apps_schedule_and_manual_selection(self) -> None:
        manual = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="workflow_dispatch",
        )
        manual_with_clippy = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="workflow_dispatch",
            enabled_boolean_inputs=frozenset({"run_clippy_all"}),
        )
        scheduled = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="schedule",
        )

        self.assertEqual(
            len(
                ci_plan.build_starry_apps_plan(manual)["starry_apps_matrix"]["include"]
            ),
            5,
        )
        self.assertEqual(
            len(
                ci_plan.build_starry_apps_plan(manual_with_clippy)[
                    "starry_apps_matrix"
                ]["include"]
            ),
            6,
        )
        self.assertEqual(
            len(
                ci_plan.build_starry_apps_plan(scheduled)["starry_apps_matrix"][
                    "include"
                ]
            ),
            6,
        )

    def test_starry_apps_manual_nixos_uses_app_runner(self) -> None:
        manual = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="workflow_dispatch",
        )

        rows = ci_plan.build_starry_apps_plan(manual)["starry_apps_matrix"]["include"]
        rows_by_id = {row["id"]: row for row in rows}

        nixos = rows_by_id["starry-nixos-x86-64-qemu"]
        self.assertEqual(nixos["container_image"], "")
        self.assertEqual(nixos["timeout_minutes"], 45)
        self.assertIn("starry app qemu -t nixos", nixos["command"])
        self.assertNotIn("starry test", nixos["command"])

    def test_catalog_has_one_artifact_producer_and_empty_self_hosted_caches(
        self,
    ) -> None:
        checks = ci_plan.load_catalog(ci_plan.MAIN_MANIFESTS)
        producers = [
            check for check in checks if check.get("upload_xtask_bin_artifact", False)
        ]
        consumers = [
            check for check in checks if check.get("download_xtask_bin_artifact", False)
        ]

        self.assertEqual([check["id"] for check in producers], ["run-sync-lint"])
        self.assertTrue(consumers)
        self.assertEqual(
            {
                check.get("xtask_bin_artifact_name", "tg-xtask-bin")
                for check in consumers
            },
            {"tg-xtask-bin"},
        )
        for check in checks:
            if "self-hosted" in check["runs_on"]:
                self.assertEqual(check.get("cache_key", ""), "", check["id"])

    def test_v3_manifests_only_reference_shared_runner_profiles(self) -> None:
        forbidden_fields = {
            "runs_on",
            "environment",
            "self_hosted_owner",
            "fallback_environment",
            "required_owner",
            "require_kvm",
        }
        manifests = (*ci_plan.MAIN_MANIFESTS, ci_plan.STARRY_APPS_MANIFEST)

        for manifest in manifests:
            with self.subTest(manifest=manifest.name):
                with manifest.open("rb") as source:
                    document = tomllib.load(source)
                self.assertEqual(document["schema_version"], 3)
                for check in document["check"]:
                    self.assertFalse(forbidden_fields.intersection(check), check["id"])

        with ci_plan.RUNNER_PROFILES_MANIFEST.open("rb") as source:
            profile_document = tomllib.load(source)
        self.assertEqual(profile_document["schema_version"], 3)

        profiles = ci_plan._load_runner_profiles(ci_plan.RUNNER_PROFILES_MANIFEST)
        self.assertEqual(
            set(profiles),
            {
                "ubuntu-base",
                "ubuntu-host",
                "ubuntu-axvisor-lvz",
                "qcs",
                "board",
                "kvm-intel",
                "kvm-amd",
            },
        )

    def test_duplicate_ids_are_rejected(self) -> None:
        manifest = ci_plan.MAIN_MANIFESTS[0]

        with self.assertRaisesRegex(ci_plan.PlanError, "duplicate check id"):
            ci_plan.load_catalog((manifest, manifest))

    def test_invalid_environment_empty_command_and_id_are_rejected(self) -> None:
        valid = {
            "id": "valid-check",
            "name": "Valid check",
            "runner": "ubuntu-base",
            "command": "true",
        }
        invalid_cases = (
            ({**valid, "command": ""}, "non-empty string"),
            ({**valid, "id": "INVALID_ID"}, "lowercase kebab-case"),
            (
                {**valid, "impact_targets": ["unknown:aarch64"]},
                "unsupported impact_targets",
            ),
            (
                {**valid, "impact_packages": ["misspelled-package"]},
                "unsupported impact_packages",
            ),
        )

        for check, error in invalid_cases:
            with (
                self.subTest(error=error),
                self.assertRaisesRegex(ci_plan.PlanError, error),
            ):
                ci_plan._validate_check(check, "test")

        with self.assertRaisesRegex(ci_plan.PlanError, "unsupported environment"):
            ci_plan._validate_runner_profile(
                {"runs_on": ["ubuntu-latest"], "environment": "unknown"},
                "test profile",
            )

        with self.assertRaisesRegex(ci_plan.PlanError, "unsupported environment"):
            ci_plan._validate_runner_profile(
                {"runs_on": ["ubuntu-latest"], "environment": ["base"]},
                "test profile",
            )

        with self.assertRaisesRegex(
            ci_plan.PlanError, "unsupported fallback_environment"
        ):
            ci_plan._validate_runner_profile(
                {
                    "runs_on": ["self-hosted"],
                    "environment": "host",
                    "self_hosted_owner": "rcore-os",
                    "fallback_environment": ["base"],
                },
                "test profile",
            )

    def test_redundant_display_names_are_rejected(self) -> None:
        valid = {
            "id": "invalid-name",
            "name": "QEMU aarch64 · Suites",
            "runner": "ubuntu-base",
            "command": "true",
        }
        invalid_cases = (
            ("Test aarch64 QEMU", "must not start"),
            ("Run Clippy", "must not start"),
            ("Check formatting", "must not start"),
            ("Scheduled Clippy", "must not start"),
            ("Board self-hosted suites", "must not expose"),
            ("aarch64 AxVisor suites", "must not repeat group"),
        )

        for name, error in invalid_cases:
            with (
                self.subTest(name=name),
                self.assertRaisesRegex(ci_plan.PlanError, error),
            ):
                ci_plan._validate_check({**valid, "name": name}, "test", "AxVisor")

    def test_starry_apps_display_names_are_leaf_labels(self) -> None:
        scheduled = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="schedule",
        )
        rows = {
            row["id"]: row["name"]
            for row in ci_plan.build_starry_apps_plan(scheduled)["starry_apps_matrix"][
                "include"
            ]
        }

        self.assertEqual(rows["starry-apps-clippy-all"], "Workspace · Full Clippy")
        self.assertEqual(rows["starry-app-smoke-x86-64"], "QEMU x86_64 · App smoke")

    def test_empty_main_matrix_is_rejected(self) -> None:
        with (
            mock.patch.object(
                ci_plan, "MAIN_MANIFESTS", (ci_plan.STARRY_APPS_MANIFEST,)
            ),
            self.assertRaisesRegex(ci_plan.PlanError, "non-empty"),
        ):
            ci_plan.build_main_plan(self.upstream)

    def test_artifact_producer_and_consumer_contract_is_enforced(self) -> None:
        producer = {
            "id": "producer",
            "phase": "static",
            "upload_xtask_bin_artifact": True,
        }
        consumer = {
            "id": "consumer",
            "phase": "test",
            "download_xtask_bin_artifact": True,
            "xtask_bin_artifact_name": "unknown-artifact",
        }

        with self.assertRaisesRegex(ci_plan.PlanError, "exactly one"):
            ci_plan._validate_artifact_contract([producer, producer])
        with self.assertRaisesRegex(ci_plan.PlanError, "unknown artifact"):
            ci_plan._validate_artifact_contract([producer, consumer])

    def test_invalid_self_hosted_cache_is_rejected(self) -> None:
        check = {
            "id": "invalid-cache",
            "name": "Invalid cache",
            "runner": "qcs",
            "cache_key": "must-not-be-set",
            "command": "true",
        }

        with self.assertRaisesRegex(ci_plan.PlanError, "empty cache_key"):
            ci_plan._validate_check(check, "test")

    def test_v3_rejects_direct_runner_fields_and_unknown_profiles(self) -> None:
        direct = {
            "id": "direct-runner",
            "name": "Direct runner",
            "runs_on": ["ubuntu-latest"],
            "environment": "base",
            "command": "true",
        }
        unknown = {
            "id": "unknown-runner",
            "name": "Unknown runner",
            "runner": "missing-profile",
            "command": "true",
        }

        with self.assertRaisesRegex(ci_plan.PlanError, "unknown fields"):
            ci_plan._validate_check(direct, "test")
        with self.assertRaisesRegex(ci_plan.PlanError, "unknown runner profile"):
            ci_plan._validate_check(unknown, "test")


if __name__ == "__main__":
    unittest.main()
