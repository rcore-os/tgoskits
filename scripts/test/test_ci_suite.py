#!/usr/bin/env python3
"""Unit tests for the ktest suite registrations of `ci_suite.py`.

Every fixture is synthetic: the temporary workspace declares its own package,
test target and config files, so the tests protect the routing and validation
rules instead of pinning the names of the migrated production cases.
"""

import importlib.util
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest

MODULE_PATH = Path(__file__).with_name("ci_suite.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("ci_suite", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ci_suite = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_suite)

DEMO_CASE_DIR = "benchmarks/axvisor/board-demo/demo"
DEMO_BUILD = f"{DEMO_CASE_DIR}/build-aarch64-unknown-none-softfloat.toml"
DEMO_BOARD = f"{DEMO_CASE_DIR}/board-demo.toml"
DEMO_CHECK_ID = "check-demo"
QEMU_CASE_DIR = "virtualization/demo/tests/case"
QEMU_BUILD = f"{QEMU_CASE_DIR}/build-aarch64-unknown-none-softfloat.toml"
QEMU_RUN = f"{QEMU_CASE_DIR}/qemu-aarch64.toml"


def write_package(root: Path, name: str, test: str, harness: bool) -> None:
    directory = root / "vendored" / name
    directory.mkdir(parents=True)
    (directory / "Cargo.toml").write_text(
        textwrap.dedent(
            f"""\
            [package]
            name = "{name}"

            [[test]]
            name = "{test}"
            path = "tests/{test}.rs"
            harness = {str(harness).lower()}
            """
        ),
        encoding="utf-8",
    )


def write_board_case(root: Path, package: str, test: str, harness: bool = False) -> None:
    """Create the legacy `board-*.toml` layout plus the ktest config files."""
    (root / DEMO_CASE_DIR).mkdir(parents=True)
    write_package(root, package, test, harness)
    (root / DEMO_BUILD).write_text(
        'target = "aarch64-unknown-none-softfloat"\n', encoding="utf-8"
    )
    (root / DEMO_BOARD).write_text('board_type = "Demo-Board"\n', encoding="utf-8")
    (root / DEMO_CASE_DIR / "guest-builds.toml").write_text(
        "arceos_build_configs = []\n", encoding="utf-8"
    )


def write_qemu_case(root: Path, package: str, test: str) -> None:
    """Create the package-owned build and QEMU configs of a ktest qemu unit."""
    (root / QEMU_CASE_DIR).mkdir(parents=True)
    write_package(root, package, test, harness=False)
    (root / QEMU_BUILD).write_text(
        'target = "aarch64-unknown-none-softfloat"\n', encoding="utf-8"
    )
    (root / QEMU_RUN).write_text("args = []\n", encoding="utf-8")


def check_with(registration: dict, *, name: str = "Board Demo · Suites") -> dict:
    return {
        "id": DEMO_CHECK_ID,
        "name": name,
        "command": (
            f"cargo xtask ktest board -p {registration.get('package')} "
            f"--test {registration.get('test')}"
        ),
        "suite": [registration],
    }


def board_registration(**overrides: object) -> dict:
    registration = {
        "kind": "ktest-board",
        "package": "demo-package",
        "test": "demo_case",
        "board": "demo",
        "config": DEMO_BUILD,
        "board_config": DEMO_BOARD,
    }
    registration.update(overrides)
    return registration


def qemu_registration(**overrides: object) -> dict:
    registration = {
        "kind": "ktest-qemu",
        "package": "demo-package",
        "test": "demo_case",
        "arch": "aarch64",
        "config": QEMU_BUILD,
        "qemu_config": QEMU_RUN,
    }
    registration.update(overrides)
    return registration


class KtestBoardSuiteTests(unittest.TestCase):
    def test_declared_config_routes_to_the_owning_check(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_board_case(root, "demo-package", "demo_case")
            check = check_with(board_registration())

            selections = ci_suite.resolve_suite_selections(root, [check], [DEMO_BOARD])

        self.assertEqual(len(selections), 1)
        self.assertEqual(selections[0].template_id, DEMO_CHECK_ID)
        self.assertEqual(selections[0].command, check["command"])

    def test_case_directory_owner_route_is_stable_across_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_board_case(root, "demo-package", "demo_case")
            check = check_with(board_registration())
            neighbour = f"{DEMO_CASE_DIR}/guest-builds.toml"

            declared = ci_suite.resolve_suite_selections(root, [check], [DEMO_BUILD])
            adjacent = ci_suite.resolve_suite_selections(root, [check], [neighbour])

        # Two files of one execution unit produce one row, not one row each.
        self.assertEqual(len(adjacent), 1)
        self.assertEqual(adjacent[0].row_id, declared[0].row_id)
        self.assertEqual(adjacent[0].source_path, neighbour)

    def test_ktest_owned_case_keeps_the_legacy_board_layout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_board_case(root, "demo-package", "demo_case")
            check = check_with(board_registration())

            # The `board-*.toml` next to the generated resources still makes the
            # directory a discoverable AxVisor board case, and the catalog stays
            # valid because the ktest registration owns that directory.
            ci_suite.validate_suite_catalog(root, [check])
            selections = ci_suite.resolve_suite_selections(root, [check], [DEMO_BOARD])

        self.assertEqual(len(selections), 1)
        self.assertEqual(selections[0].template_id, DEMO_CHECK_ID)

    def test_unknown_package_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_board_case(root, "demo-package", "demo_case")
            check = check_with(board_registration(package="missing-package"))

            with self.assertRaises(ci_suite.SuiteRouteError) as caught:
                ci_suite.validate_suite_catalog(root, [check])

        self.assertIn("missing-package", str(caught.exception))

    def test_target_must_be_a_harness_false_test(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_board_case(root, "demo-package", "demo_case", harness=True)

            with self.assertRaises(ci_suite.SuiteRouteError) as caught:
                ci_suite.validate_suite_catalog(root, [check_with(board_registration())])

        self.assertIn("demo_case", str(caught.exception))

    def test_missing_config_file_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_board_case(root, "demo-package", "demo_case")
            check = check_with(
                board_registration(board_config=f"{DEMO_CASE_DIR}/board-absent.toml")
            )

            with self.assertRaises(ci_suite.SuiteRouteError) as caught:
                ci_suite.validate_suite_catalog(root, [check])

        self.assertIn("board-absent.toml", str(caught.exception))

    def test_unowned_suite_path_fails_instead_of_being_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_board_case(root, "demo-package", "demo_case")

            # A discoverable case without any registration must fail the plan
            # instead of silently dropping the change.
            with self.assertRaises(ci_suite.SuiteRouteError):
                ci_suite.resolve_suite_selections(root, [], [DEMO_BOARD])


class KtestQemuSuiteTests(unittest.TestCase):
    def test_arch_must_match_the_build_target(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_qemu_case(root, "demo-package", "demo_case")
            check = check_with(qemu_registration(arch="x86_64"), name="QEMU demo")

            with self.assertRaises(ci_suite.SuiteRouteError) as caught:
                ci_suite.validate_suite_catalog(root, [check])

        self.assertIn("x86_64", str(caught.exception))

    def test_matching_arch_validates_and_routes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_qemu_case(root, "demo-package", "demo_case")
            check = check_with(qemu_registration(), name="QEMU demo")

            ci_suite.validate_suite_catalog(root, [check])
            selections = ci_suite.resolve_suite_selections(root, [check], [QEMU_RUN])

        self.assertEqual(len(selections), 1)
        self.assertEqual(selections[0].template_id, DEMO_CHECK_ID)



class CpuManifestRoutingTests(unittest.TestCase):
    def make_workspace(self, root: Path) -> list[dict]:
        cpu = root / "test-suit/arceos/cpu"
        (cpu / "normal").mkdir(parents=True)
        (cpu / "Cargo.toml").write_text('[package]\nname = "cpu-fixture"\n[features]\nio = []\n')
        declarations = ['schema_version = 1', 'run_order = ["alpha", "beta"]']
        checks = []
        for name, arch in [("alpha", "aarch64"), ("beta", "x86_64")]:
            directory = cpu / "normal/configs" / arch / name
            directory.mkdir(parents=True)
            (directory / "build.toml").write_text('features = []\n')
            (directory / "qemu.toml").write_text('args = []\n')
            declarations.append(f"""
[batches.{name}]
arch = "{arch}"
image = "std"
mode = "normal"
build_config = "configs/{arch}/{name}/build.toml"
qemu_config = "configs/{arch}/{name}/qemu.toml"
[[batches.{name}.tests]]
id = "io"
name = "real I/O capability"
entry = "cpu_tests::io::run"
""")
            checks.append({"id": f"cpu-{arch}", "name": f"CPU {arch}", "command": "fixture", "suite": [
                {"kind": "arceos-qemu", "arch": arch, "group": "cpu"}]})
        (cpu / "normal/suite.toml").write_text("\n".join(declarations))
        board = root / "test-suit/arceos/board-demo/probe"
        board.mkdir(parents=True)
        (board / "build-aarch64-unknown-none-softfloat.toml").write_text(
            'package = "arceos-cpu-normal-std"\ntarget = "aarch64-unknown-none-softfloat"\n')
        (board / "board-demo.toml").write_text('board_type = "Demo"\n')
        (board.parent / "suite.toml").write_text('''
schema_version = 1
board = "demo"
run_order = ["probe"]
[batches.probe]
arch = "aarch64"
image = "std"
executor = "cpu"
package = "arceos-cpu-normal-std"
build_config = "probe/build-aarch64-unknown-none-softfloat.toml"
board_config = "probe/board-demo.toml"
[[batches.probe.tests]]
id = "io"
name = "board I/O capability"
entry = "cpu_tests::io::run"
returns = "unit"
features = ["cpu-tests/io"]
''')
        checks.append({"id": "board-io", "name": "Board I/O", "command": "fixture", "suite": [
            {"kind": "arceos-board", "board": "demo", "cases": ["probe"]}]})
        return checks

    def test_shared_capability_selects_declared_qemu_and_board_consumers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checks = self.make_workspace(root)
            ci_suite.validate_suite_catalog(root, checks)
            selections = ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/cpu/src/io.rs"])
            self.assertEqual({item.template_id for item in selections}, {"cpu-aarch64", "cpu-x86_64", "board-io"})
            self.assertTrue(any("--test-case alpha" in item.command for item in selections))
            with self.assertRaises(ci_suite.SuiteRouteError):
                ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/retired-platform/io.rs"])
            scoped = ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/cpu/normal/configs/x86_64/beta/qemu.toml"])
            self.assertEqual([item.template_id for item in scoped], ["cpu-x86_64"])
            entry = ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/cpu/normal/main.rs"])
            self.assertEqual({item.template_id for item in entry}, {"cpu-aarch64", "cpu-x86_64", "board-io"})
            std_entry = ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/cpu/normal/std/Cargo.toml"])
            self.assertIn("board-io", {item.template_id for item in std_entry})

    def test_board_scoped_source_routes_only_its_board_consumers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checks = self.make_workspace(root)
            for source in ["src/board/demo/io.rs", "src/board/demo/mod.rs", "src/board/mod.rs"]:
                selections = ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/cpu/" + source])
                self.assertEqual({item.template_id for item in selections}, {"board-io"})
            with self.assertRaises(ci_suite.SuiteRouteError):
                ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/cpu/src/board/other/io.rs"])

    def test_board_manifest_routes_shared_changes_without_directory_duplicates(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checks = self.make_workspace(root)
            source = "test-suit/arceos/board-demo/suite.toml"
            selections = ci_suite.resolve_suite_selections(root, checks, [source])
            self.assertEqual(len(selections), 1)
            self.assertIn("--test-case probe", selections[0].command)
            manifest = root / source
            original = manifest.read_text()
            for invalid in [original.replace('run_order = ["probe"]', 'run_order = []'),
                    original.replace('returns = "unit"', 'returns = "result"'),
                    original.replace('features = ["cpu-tests/io"]', 'features = "cpu-tests/io"')]:
                manifest.write_text(invalid)
                with self.assertRaises(ci_suite.SuiteRouteError):
                    ci_suite.validate_suite_catalog(root, checks)

    def test_rust_member_source_routes_board_batch_while_unrelated_sources_do_not(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checks = self.make_workspace(root)
            manifest = root / "test-suit/arceos/board-demo/suite.toml"
            manifest.write_text(manifest.read_text().replace('executor = "cpu"', 'executor = "rust"')
                .replace('entry = "cpu_tests::io::run"', 'entry = "crate::task::delivery::run"')
                .replace('returns = "unit"', 'returns = "result"'))
            selections = ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/rust/src/task/delivery.rs"])
            self.assertEqual([item.template_id for item in selections], ["board-io"])
            with self.assertRaises(ci_suite.SuiteRouteError):
                ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/rust/src/task/unrelated.rs"])

    def test_missing_cpu_runtime_is_rejected_and_arch_specific_sources_stay_scoped(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checks = self.make_workspace(root)
            selections = ci_suite.resolve_suite_selections(root, checks, ["test-suit/arceos/cpu/src/io/aarch64.rs"])
            self.assertEqual({item.template_id for item in selections}, {"cpu-aarch64", "board-io"})
            (root / "test-suit/arceos/cpu/normal/configs/x86_64/beta/qemu.toml").unlink()
            with self.assertRaises(ci_suite.SuiteRouteError):
                ci_suite.validate_suite_catalog(root, checks)

class DriverBatchRoutingTests(unittest.TestCase):
    def test_shared_driver_image_routes_members_and_rejects_missing_output_fixture(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            suite=root/"test-suit/arceos/drivers"
            suite.mkdir(parents=True)
            (suite/"build.toml").write_text('freestanding = true\n')
            runtime=suite/"qemu.toml"
            runtime.write_text('[host_pcm_waveform]\nframes=64\n')
            manifest=suite/"suite.toml"
            manifest.write_text('''
schema_version=1
run_order=["combined"]
[batches.combined]
arch="x86_64"
build_config="build.toml"
qemu_config="qemu.toml"
[[batches.combined.tests]]
id="parse"
entry="crate::parse::run"
[[batches.combined.tests]]
id="play"
entry="crate::play::run"
pcm_waveform=true
''')
            checks=[{"id":"driver-fixture","name":"Driver fixture","command":"fixture","suite":[{"kind":"arceos-qemu","arch":"x86_64","group":"drivers"}]}]
            ci_suite.validate_suite_catalog(root,checks)
            rows=ci_suite.resolve_suite_selections(root,checks,["test-suit/arceos/drivers/src/parse.rs","test-suit/arceos/drivers/src/play.rs"])
            self.assertEqual(len(rows),1)
            self.assertIn("--test-group drivers --test-case combined",rows[0].command)
            runtime.write_text('args=[]\n')
            with self.assertRaises(ci_suite.SuiteRouteError):ci_suite.validate_suite_catalog(root,checks)

class ClassifiedStarryRoutingTests(unittest.TestCase):
    def test_removed_arm_boot_path_routes_to_the_retained_composite(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wrapper = root / "test-suit/starryos/qemu/standard"
            composite = wrapper / "openrc"
            composite.mkdir(parents=True)
            (wrapper / "build-aarch64-unknown-none-softfloat.toml").write_text('target="aarch64-unknown-none-softfloat"\n')
            (composite / "qemu-aarch64.toml").write_text('args=[]\n')
            sibling = wrapper / "independent-probe"
            sibling.mkdir()
            (sibling / "qemu-aarch64.toml").write_text('args=[]\n')
            checks = [{"id": "fixture-arm", "name": "Fixture", "command": "fixture",
                "suite": [{"kind": "starry-qemu", "arch": "aarch64"}]}]
            paths = ["test-suit/starryos/qemu/gicv2-boot/qemu-aarch64.toml",
                "test-suit/starryos/qemu/standard/gicv2-boot/qemu-aarch64.toml",
                "test-suit/starryos/qemu/standard/openrc/qemu-aarch64.toml"]
            rows = ci_suite.resolve_suite_selections(root, checks, paths)
            self.assertEqual(len(rows), 1)
            self.assertIn("--test-case qemu/openrc", rows[0].command)

    def test_board_wrapper_routes_keep_public_ids_after_layout_moves(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wrapper = root / "test-suit/starryos/board/fixture/probe"
            wrapper.mkdir(parents=True)
            (wrapper / "build-x86_64-unknown-none.toml").write_text('target="x86_64-unknown-none"\n')
            (wrapper / "board-fixture.toml").write_text('board_type="Fixture"\n')
            checks = [{"id": "fixture-board", "name": "Fixture", "command": "fixture",
                "suite": [{"kind": "starry-board", "board": "fixture", "cases": ["board-fixture/probe"]}]}]
            ci_suite.validate_suite_catalog(root, checks)
            for path in ["board-fixture/probe", "board/fixture/probe"]:
                rows = ci_suite.resolve_suite_selections(root, checks, [f"test-suit/starryos/{path}/board-fixture.toml"])
                self.assertEqual(len(rows), 1)
                self.assertIn("--test-case board-fixture/probe", rows[0].command)

    def test_classified_project_routes_old_and_new_paths_to_one_selection(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wrapper = root / "test-suit/starryos/qemu/standard"
            project = wrapper / "system/memory/buffer-check"
            project.mkdir(parents=True)
            (wrapper / "build-x86_64-unknown-none.toml").write_text('target="x86_64-unknown-none"\n')
            (wrapper / "system/qemu-x86_64.toml").write_text('test_commands=["exec runner"]\n')
            (project / "CMakeLists.txt").write_text('add_executable(buffer-check src/main.c)\n')
            (project / "vendor").mkdir()
            (project / "vendor/CMakeLists.txt").write_text('project(dependency C)\n')
            checks = [{"id": "fixture-starry", "name": "Fixture", "command": "fixture",
                "suite": [{"kind": "starry-qemu", "arch": "x86_64"}]}]
            ci_suite.validate_suite_catalog(root, checks)
            selections = ci_suite.resolve_suite_selections(root, checks, [
                "test-suit/starryos/qemu/standard/system/memory/buffer-check/src/main.c",
                "test-suit/starryos/qemu/system/buffer-check/src/main.c",
                "test-suit/starryos/qemu/standard/system/memory/buffer-check/vendor/source.c",
            ])
            self.assertEqual(len(selections), 1)
            self.assertIn("--test-case qemu/buffer-check", selections[0].command)

    def test_shared_c_project_also_routes_the_board_that_builds_its_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            suite = root / "test-suit/starryos"
            wrapper = suite / "qemu/standard"
            project = wrapper / "system/devices/device-check"
            (project / "src").mkdir(parents=True)
            (project / "src/main.c").write_text("fixture")
            (project / "CMakeLists.txt").write_text("project(device-check C)\n")
            (wrapper / "build-aarch64-unknown-none-softfloat.toml").write_text('target="aarch64-unknown-none-softfloat"\n')
            (wrapper / "system/qemu-aarch64.toml").write_text('test_commands=["exec runner"]\n')
            board = suite / "board/fixture"
            case = board / "probe"
            (case / "c").mkdir(parents=True)
            (board / "build-aarch64-unknown-none-softfloat.toml").write_text('target="aarch64-unknown-none-softfloat"\n')
            (case / "board-fixture.toml").write_text('board_type="Fixture"\n')
            (case / "c/CMakeLists.txt").write_text('add_executable(check ../../../../qemu/standard/system/devices/device-check/src/main.c)\n')
            checks = [
                {"id": "fixture-qemu", "name": "Fixture", "command": "fixture", "suite": [{"kind": "starry-qemu", "arch": "aarch64"}]},
                {"id": "fixture-board", "name": "Fixture", "command": "fixture", "suite": [{"kind": "starry-board", "board": "fixture"}]},
            ]
            rows = ci_suite.resolve_suite_selections(root, checks, ["test-suit/starryos/qemu/standard/system/devices/device-check/src/main.c"])
            self.assertEqual({row.template_id for row in rows}, {"fixture-qemu", "fixture-board"})
            self.assertTrue(any("--test-case probe" in row.command for row in rows))

    def test_common_network_sources_route_only_to_registered_board_consumers(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wrapper = root / "test-suit/starryos/board/fixture"
            case = wrapper / "probe"
            case.mkdir(parents=True)
            (wrapper / "build-aarch64-unknown-none-softfloat.toml").write_text('target="aarch64-unknown-none-softfloat"\n')
            (case / "board-fixture.toml").write_text('board_type="Fixture"\n')
            checks = [{"id": "fixture-board", "name": "Fixture", "command": "fixture",
                "suite": [{"kind": "starry-board", "board": "fixture"}]}]
            source = "test-suit/starryos/common/network/upload.c"
            (root / source).parent.mkdir(parents=True)
            (root / source).write_text("fixture")
            selections = ci_suite.resolve_suite_selections(root, checks, [source])
            self.assertEqual({item.template_id for item in selections}, {"fixture-board"})

if __name__ == "__main__":
    unittest.main()
