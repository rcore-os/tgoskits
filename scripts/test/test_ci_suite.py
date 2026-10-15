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


if __name__ == "__main__":
    unittest.main()
