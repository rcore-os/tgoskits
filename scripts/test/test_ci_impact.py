#!/usr/bin/env python3

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock


MODULE_PATH = Path(__file__).with_name("ci_impact.py")
SPEC = importlib.util.spec_from_file_location("ci_impact", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ci_impact = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_impact)


class CiImpactTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.workspace_root = Path(self.temp_dir.name)
        package_dirs = {
            "shared": "components/shared",
            "arm-vcpu": "virtualization/arm-vcpu",
            "standalone": "tools/standalone",
            "axtest": "components/axtest/axtest",
            "ktest-only": "components/ktest-only",
            "arceos-test-suit": "test-suit/arceos/rust",
            "starryos": "os/StarryOS/starryos",
            "axvisor": "os/axvisor",
        }
        self.packages = []
        for package, relative_dir in package_dirs.items():
            manifest = self.workspace_root / relative_dir / "Cargo.toml"
            manifest.parent.mkdir(parents=True, exist_ok=True)
            manifest.write_text("[package]\n", encoding="utf-8")
            package_metadata = {}
            if package == "ktest-only":
                package_metadata = {
                    "metadata": {
                        "docs": {
                            "rs": {
                                "targets": [
                                    ci_impact.ARCH_TARGETS["x86_64"],
                                    ci_impact.ARCH_TARGETS["aarch64"],
                                ]
                            }
                        }
                    }
                }
            self.packages.append(
                {
                    "id": f"path+file:///{package}#0.1.0",
                    "name": package,
                    "manifest_path": str(manifest),
                    **package_metadata,
                }
            )

        self.package_ids = {
            package["name"]: package["id"] for package in self.packages
        }
        self.metadata_by_arch = {
            arch: self._metadata(arch) for arch in ci_impact.ARCH_TARGETS
        }

    def _metadata(self, arch: str) -> dict:
        shared = self.package_ids["shared"]
        arm_vcpu = self.package_ids["arm-vcpu"]
        dependency_names = {
            "arceos-test-suit": ["shared"],
            "starryos": ["shared"],
            "axvisor": ["shared"] + (["arm-vcpu"] if arch == "aarch64" else []),
            "ktest-only": ["axtest"],
        }
        nodes = []
        for package in self.packages:
            deps = []
            for name in dependency_names.get(package["name"], []):
                dep_kinds = [{"kind": "dev", "target": None}] if name == "axtest" else []
                deps.append({"pkg": self.package_ids[name], "dep_kinds": dep_kinds})
            nodes.append({"id": package["id"], "deps": deps})
        self.assertIn(shared, {node["id"] for node in nodes})
        self.assertIn(arm_vcpu, {node["id"] for node in nodes})
        return {
            "workspace_root": str(self.workspace_root),
            "workspace_members": [package["id"] for package in self.packages],
            "packages": self.packages,
            "resolve": {"nodes": nodes},
        }

    def test_shared_crate_selects_every_os_on_every_arch(self) -> None:
        impact = ci_impact.analyze_changed_paths(
            self.workspace_root,
            [Path("components/shared/src/lib.rs")],
            self.metadata_by_arch,
        )

        self.assertFalse(impact.full)
        self.assertEqual(impact.changed_packages, ("shared",))
        self.assertEqual(
            impact.affected_packages,
            ("arceos-test-suit", "axvisor", "shared", "starryos"),
        )
        self.assertEqual(
            set(impact.targets),
            {
                f"{os_name}:{arch}"
                for os_name in ("arceos", "starry", "axvisor")
                for arch in ci_impact.ARCH_TARGETS
            },
        )

    def test_markdown_does_not_expand_a_mixed_code_change(self) -> None:
        impact = ci_impact.analyze_changed_paths(
            self.workspace_root,
            [
                Path("components/shared/src/lib.rs"),
                Path("virtualization/arm-vcpu/README.md"),
            ],
            self.metadata_by_arch,
        )

        self.assertFalse(impact.full)
        self.assertEqual(impact.changed_packages, ("shared",))
        self.assertEqual(
            impact.ignored_markdown,
            ("virtualization/arm-vcpu/README.md",),
        )
        self.assertEqual(
            set(impact.targets),
            {
                f"{os_name}:{arch}"
                for os_name in ("arceos", "starry", "axvisor")
                for arch in ci_impact.ARCH_TARGETS
            },
        )

    def test_target_specific_crate_selects_only_matching_target(self) -> None:
        impact = ci_impact.analyze_changed_paths(
            self.workspace_root,
            [Path("virtualization/arm-vcpu/src/lib.rs")],
            self.metadata_by_arch,
        )

        self.assertFalse(impact.full)
        self.assertEqual(impact.targets, ("axvisor:aarch64",))

    def test_standalone_crate_does_not_select_an_os(self) -> None:
        impact = ci_impact.analyze_changed_paths(
            self.workspace_root,
            [Path("tools/standalone/src/main.rs")],
            self.metadata_by_arch,
        )

        self.assertFalse(impact.full)
        self.assertEqual(impact.changed_packages, ("standalone",))
        self.assertEqual(impact.affected_packages, ("standalone",))
        self.assertEqual(impact.targets, ())

    def test_standalone_axtest_package_selects_its_ci_architectures(self) -> None:
        impact = ci_impact.analyze_changed_paths(
            self.workspace_root,
            [Path("components/ktest-only/src/lib.rs")],
            self.metadata_by_arch,
        )

        self.assertFalse(impact.full)
        self.assertEqual(
            impact.targets,
            ("arceos:aarch64", "arceos:x86_64"),
        )

    def test_markdown_and_apps_do_not_expand_runtime_impact(self) -> None:
        impact = ci_impact.analyze_changed_paths(
            self.workspace_root,
            [
                Path("components/shared/README.md"),
                Path("apps/starry/demo/prebuild.sh"),
            ],
            self.metadata_by_arch,
        )

        self.assertFalse(impact.full)
        self.assertEqual(impact.targets, ())
        self.assertEqual(impact.ignored_markdown, ("components/shared/README.md",))
        self.assertEqual(impact.ignored_apps, ("apps/starry/demo/prebuild.sh",))

    def test_known_non_package_path_uses_os_and_arch_hint(self) -> None:
        impact = ci_impact.analyze_changed_paths(
            self.workspace_root,
            [Path("test-suit/starryos/qemu-aarch64.toml")],
            self.metadata_by_arch,
        )

        self.assertFalse(impact.full)
        self.assertEqual(impact.targets, ("starry:aarch64",))

    def test_os_specific_config_without_arch_hint_selects_all_os_arches(self) -> None:
        impact = ci_impact.analyze_changed_paths(
            self.workspace_root,
            [Path("os/arceos/configs/defconfig.toml")],
            self.metadata_by_arch,
        )

        self.assertFalse(impact.full)
        self.assertEqual(
            impact.targets,
            tuple(f"arceos:{arch}" for arch in ci_impact.ARCH_TARGETS),
        )

    def test_known_config_names_map_to_board_architectures(self) -> None:
        cases = {
            "os/StarryOS/configs/board/visionfive2.toml": ("starry:riscv64",),
            "os/StarryOS/configs/board/jl-lsgd2k10.toml": (
                "starry:loongarch64",
            ),
            "os/axvisor/configs/board/asus-nuc15crh-x86_64.toml": (
                "axvisor:x86_64",
            ),
            "os/axvisor/configs/board/orangepi-5-plus.toml": (
                "axvisor:aarch64",
            ),
        }
        for changed_path, expected_targets in cases.items():
            with self.subTest(path=changed_path):
                impact = ci_impact.analyze_changed_paths(
                    self.workspace_root,
                    [Path(changed_path)],
                    self.metadata_by_arch,
                )

                self.assertFalse(impact.full)
                self.assertEqual(impact.targets, expected_targets)

    def test_deleted_package_manifest_falls_back_to_full(self) -> None:
        manifest = self.workspace_root / "tools/standalone/Cargo.toml"
        manifest.unlink()

        impact = ci_impact.analyze_changed_paths(
            self.workspace_root,
            [Path("tools/standalone/Cargo.toml")],
            self.metadata_by_arch,
        )

        self.assertTrue(impact.full)
        self.assertIn("was deleted", impact.reason)

    def test_diff_and_metadata_failures_fall_back_to_full(self) -> None:
        with mock.patch.object(
            ci_impact,
            "changed_paths_since",
            side_effect=ci_impact.subprocess.CalledProcessError(1, ["git", "diff"]),
        ):
            diff_failure = ci_impact.analyze_pull_request(self.workspace_root, "base")

        with (
            mock.patch.object(
                ci_impact,
                "changed_paths_since",
                return_value=[
                    Path("components/shared/src/lib.rs"),
                    Path("components/shared/README.md"),
                ],
            ),
            mock.patch.object(
                ci_impact,
                "load_metadata_by_arch",
                side_effect=OSError("metadata unavailable"),
            ),
        ):
            metadata_failure = ci_impact.analyze_pull_request(
                self.workspace_root, "base"
            )

        self.assertTrue(diff_failure.full)
        self.assertTrue(metadata_failure.full)
        self.assertEqual(
            metadata_failure.ignored_markdown,
            ("components/shared/README.md",),
        )
        self.assertEqual(len(metadata_failure.targets), 12)

    def test_global_change_skips_unneeded_metadata_loading(self) -> None:
        with (
            mock.patch.object(
                ci_impact,
                "changed_paths_since",
                return_value=[Path("scripts/test/ci_impact.py")],
            ),
            mock.patch.object(ci_impact, "load_metadata_by_arch") as load_metadata,
        ):
            impact = ci_impact.analyze_pull_request(self.workspace_root, "base")

        self.assertTrue(impact.full)
        load_metadata.assert_not_called()

    def test_summary_reports_selected_and_skipped_checks(self) -> None:
        impact = ci_impact.CiImpact(
            full=False,
            reason="fixture",
            changed_paths=("components/shared/src/lib.rs",),
            ignored_markdown=("components/shared/README.md",),
            changed_packages=("shared",),
            affected_packages=("shared", "starryos"),
            targets=("starry:aarch64",),
        )

        summary = ci_impact.render_summary(
            impact,
            ["run-clippy", "test-starry-aarch64-qemu"],
            ["test-starry-x86-64-qemu"],
        )

        self.assertIn("components/shared/src/lib.rs", summary)
        self.assertIn("components/shared/README.md", summary)
        self.assertIn("starry:aarch64", summary)
        self.assertIn("Selected checks (2)", summary)
        self.assertIn("Skipped checks (1)", summary)

    def test_unknown_and_global_paths_fall_back_to_full(self) -> None:
        for changed_path in (
            "unknown/input.bin",
            "Cargo.toml",
            "Cargo.lock",
            ".cargo/config.toml",
            "scripts/axbuild/src/lib.rs",
            "scripts/test/ci_plan.py",
            "xtask/src/main.rs",
            ".github/ci/checks/starry.toml",
        ):
            with self.subTest(path=changed_path):
                impact = ci_impact.analyze_changed_paths(
                    self.workspace_root,
                    [Path(changed_path)],
                    self.metadata_by_arch,
                )

                self.assertTrue(impact.full)
                self.assertTrue(impact.reason)


if __name__ == "__main__":
    unittest.main()
