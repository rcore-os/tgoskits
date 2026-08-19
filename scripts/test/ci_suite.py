#!/usr/bin/env python3

import re
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ci_impact import ARCH_TARGETS

SUPPORTED_SUITE_KINDS = {
    "arceos-qemu",
    "starry-qemu",
    "starry-board",
    "axvisor-qemu",
    "axvisor-board",
}
SUITE_FIELDS = {"kind", "arch", "board", "cases"}
SUITE_ROOTS = {
    "arceos": Path("test-suit/arceos"),
    "starry": Path("test-suit/starryos"),
    "axvisor": Path("test-suit/axvisor"),
}


class SuiteRouteError(ValueError):
    """Raised when a changed test suite cannot map to one registered CI row."""


@dataclass(frozen=True)
class SuiteSelection:
    template_id: str
    row_id: str
    leaf_name: str
    command: str
    source_path: str


@dataclass(frozen=True)
class _RuntimeCase:
    kind: str
    arch: str | None
    board: str | None
    case: str
    case_dir: Path
    wrapper_dir: Path
    runtime_config: Path
    build_config: Path


def resolve_suite_selections(
    workspace_root: Path,
    checks: Sequence[dict[str, Any]],
    changed_paths: Iterable[str],
) -> list[SuiteSelection]:
    registrations = _suite_registrations(checks)
    selections: dict[tuple[str, str, str], SuiteSelection] = {}
    check_order = {check["id"]: index for index, check in enumerate(checks)}

    for rendered_path in sorted(set(changed_paths)):
        path = Path(rendered_path)
        path_selections = _selections_for_path(
            workspace_root,
            registrations,
            path,
        )
        if not path_selections:
            raise SuiteRouteError(
                f"test suite path `{rendered_path}` is not registered in CI"
            )
        for selection in path_selections:
            key = (selection.template_id, selection.leaf_name, selection.command)
            selections.setdefault(key, selection)

    return sorted(
        selections.values(),
        key=lambda selection: (
            check_order[selection.template_id],
            selection.leaf_name,
            selection.command,
        ),
    )


def validate_suite_catalog(
    workspace_root: Path,
    checks: Sequence[dict[str, Any]],
) -> None:
    registrations = _suite_registrations(checks)
    discovered = {
        "starry": _discover_runtime_cases(
            workspace_root / SUITE_ROOTS["starry"],
            "starry",
        ),
        "axvisor": _discover_runtime_cases(
            workspace_root / SUITE_ROOTS["axvisor"],
            "axvisor",
        ),
    }
    for check, registration in registrations:
        kind = registration["kind"]
        if kind == "arceos-qemu":
            arch = registration["arch"]
            runtime = (
                workspace_root / SUITE_ROOTS["arceos"] / "rust" / f"qemu-{arch}.toml"
            )
            build = (
                workspace_root
                / SUITE_ROOTS["arceos"]
                / "rust"
                / f"build-{ARCH_TARGETS[arch]}.toml"
            )
            if runtime.is_file() and build.is_file():
                continue
            raise SuiteRouteError(
                f"check '{check['id']}' registers missing ArceOS QEMU capability {arch}"
            )

        candidates = [
            runtime_case
            for runtime_case in discovered[_kind_os(kind)]
            if runtime_case.kind == kind
            and (
                registration.get("arch") is None
                or runtime_case.arch == registration["arch"]
            )
            and (
                registration.get("board") is None
                or runtime_case.board == registration["board"]
            )
        ]
        registered_cases = registration.get("cases")
        if registered_cases:
            missing = set(registered_cases) - {
                runtime_case.case for runtime_case in candidates
            }
            if missing:
                raise SuiteRouteError(
                    f"check '{check['id']}' registers missing suite cases: {sorted(missing)}"
                )
        elif not candidates:
            raise SuiteRouteError(
                f"check '{check['id']}' registers a suite capability with no runtime cases"
            )

    for runtime_cases in discovered.values():
        for runtime_case in runtime_cases:
            _registered_template(
                registrations,
                kind=runtime_case.kind,
                arch=runtime_case.arch,
                board=runtime_case.board,
                case=runtime_case.case,
            )


def check_matches_input(check: dict[str, Any], selection: str) -> bool:
    os_name, separator, remainder = selection.partition(":")
    if not separator:
        return False
    registrations = check.get("suite", ())
    if remainder == "all":
        return any(
            _kind_os(registration["kind"]) == os_name for registration in registrations
        )

    platform, separator, value = remainder.partition(":")
    if not separator:
        return False
    for registration in registrations:
        kind = registration["kind"]
        if _kind_os(kind) != os_name:
            continue
        if (
            platform == "qemu"
            and kind.endswith("-qemu")
            and registration.get("arch") == value
        ):
            return True
        if platform == "board" and kind.endswith("-board"):
            board = registration.get("board", "")
            if (
                board == value
                or board.startswith(f"{value}-")
                or value.startswith(f"{board}-")
            ):
                return True
    return False


def _selections_for_path(
    workspace_root: Path,
    registrations: Sequence[tuple[dict[str, Any], dict[str, Any]]],
    path: Path,
) -> list[SuiteSelection]:
    if _is_prefix(path, SUITE_ROOTS["arceos"]):
        return _arceos_selections(registrations, path)
    if _is_prefix(path, SUITE_ROOTS["starry"]):
        return _discovered_selections(
            workspace_root,
            registrations,
            path,
            "starry",
        )
    if _is_prefix(path, SUITE_ROOTS["axvisor"]):
        return _discovered_selections(
            workspace_root,
            registrations,
            path,
            "axvisor",
        )
    return []


def _arceos_selections(
    registrations: Sequence[tuple[dict[str, Any], dict[str, Any]]],
    path: Path,
) -> list[SuiteSelection]:
    root = SUITE_ROOTS["arceos"]
    relative = path.relative_to(root)
    if not relative.parts:
        return _arceos_all_selections(registrations, path)

    group = relative.parts[0]
    if group not in {"rust", "c", "loongarch"}:
        return []

    case = None
    display_case = group
    if group == "rust" and len(relative.parts) >= 3 and relative.parts[1] == "cases":
        case = relative.parts[2]
        display_case = f"rust/{case}"
    elif group == "loongarch" and len(relative.parts) >= 2:
        case = relative.parts[1]
        display_case = f"loongarch/{case}"

    arches = _arches_for_path(path)
    if not arches and group == "loongarch":
        arches = {"loongarch64"}
    if not arches:
        arches = {
            registration["arch"]
            for _, registration in registrations
            if registration["kind"] == "arceos-qemu"
        }

    selections = []
    for arch in sorted(arches, key=_arch_sort_key):
        template = _registered_template(
            registrations,
            kind="arceos-qemu",
            arch=arch,
            case=case,
        )
        if template is None:
            continue
        command = f"cargo xtask arceos test qemu --arch {arch} --test-group {group}"
        if case is not None:
            command += f" --test-case {case}"
        selections.append(
            _selection(
                template,
                f"QEMU {arch}",
                display_case,
                command,
                path,
            )
        )
    return selections


def _arceos_all_selections(
    registrations: Sequence[tuple[dict[str, Any], dict[str, Any]]],
    path: Path,
) -> list[SuiteSelection]:
    selections = []
    for check, registration in registrations:
        if registration["kind"] != "arceos-qemu":
            continue
        arch = registration["arch"]
        selections.append(
            _selection(
                check,
                f"QEMU {arch}",
                "all",
                f"cargo xtask arceos test qemu --arch {arch}",
                path,
            )
        )
    return selections


def _discovered_selections(
    workspace_root: Path,
    registrations: Sequence[tuple[dict[str, Any], dict[str, Any]]],
    path: Path,
    os_name: str,
) -> list[SuiteSelection]:
    root = SUITE_ROOTS[os_name]
    absolute_root = workspace_root / root
    absolute_path = workspace_root / path
    cases = _discover_runtime_cases(absolute_root, os_name)

    grouped = _starry_grouped_subcase(absolute_root, absolute_path, cases)
    if grouped is not None:
        matching_cases, selector = grouped
        return _runtime_selections(
            registrations,
            path,
            matching_cases,
            selector_override=selector,
        )

    matching_cases = _matching_runtime_cases(cases, absolute_path)
    return _runtime_selections(registrations, path, matching_cases)


def _runtime_selections(
    registrations: Sequence[tuple[dict[str, Any], dict[str, Any]]],
    path: Path,
    cases: Sequence[_RuntimeCase],
    *,
    selector_override: str | None = None,
) -> list[SuiteSelection]:
    selections = []
    for runtime_case in cases:
        selector = selector_override or runtime_case.case
        template = _registered_template(
            registrations,
            kind=runtime_case.kind,
            arch=runtime_case.arch,
            board=runtime_case.board,
            case=runtime_case.case,
        )
        if template is None:
            continue

        if runtime_case.kind == "starry-qemu":
            platform = f"QEMU {runtime_case.arch}"
            command = (
                f"cargo xtask starry test qemu --arch {runtime_case.arch} "
                f"--test-case {selector}"
            )
        elif runtime_case.kind == "axvisor-qemu":
            variant = _case_variant(runtime_case.case)
            platform = (
                f"{variant.upper()} {runtime_case.arch}"
                if variant in {"vmx", "svm"}
                else f"QEMU {runtime_case.arch}"
            )
            command = (
                f"cargo xtask axvisor test qemu --arch {runtime_case.arch} "
                f"--test-group normal --test-case {selector}"
            )
        else:
            platform = _platform_label(template)
            os_cli = "starry" if runtime_case.kind == "starry-board" else "axvisor"
            command = (
                f"cargo xtask {os_cli} test board --test-case {selector} "
                f"--board {runtime_case.board}"
            )
            if runtime_case.kind == "axvisor-board":
                command = command.replace(
                    " test board",
                    " test board --test-group normal",
                    1,
                )

        selections.append(_selection(template, platform, selector, command, path))
    return selections


def _discover_runtime_cases(root: Path, os_name: str) -> list[_RuntimeCase]:
    cases = []
    qemu_kind = f"{os_name}-qemu"
    board_kind = f"{os_name}-board"
    for runtime_config in sorted(root.rglob("qemu-*.toml")):
        parsed = _parse_qemu_config_name(runtime_config.stem)
        if parsed is None:
            continue
        arch, variant = parsed
        build_config = _nearest_qemu_build_config(
            runtime_config.parent,
            root,
            arch,
            variant,
        )
        if build_config is None:
            continue
        wrapper_dir = build_config.parent
        if os_name == "starry":
            case = runtime_config.parent.relative_to(root).as_posix()
        else:
            base_case = (
                wrapper_dir.name
                if runtime_config.parent == wrapper_dir
                else runtime_config.parent.name
            )
            case = f"{base_case}-{variant}" if variant else base_case
        cases.append(
            _RuntimeCase(
                kind=qemu_kind,
                arch=arch,
                board=None,
                case=case,
                case_dir=runtime_config.parent,
                wrapper_dir=wrapper_dir,
                runtime_config=runtime_config,
                build_config=build_config,
            )
        )

    for runtime_config in sorted(root.rglob("board-*.toml")):
        build_config = _nearest_board_build_config(runtime_config.parent, root)
        if build_config is None:
            continue
        wrapper_dir = build_config.parent
        relative_case = runtime_config.parent.relative_to(wrapper_dir)
        case = (
            relative_case.as_posix()
            if relative_case.parts
            else wrapper_dir.relative_to(root).as_posix()
        )
        cases.append(
            _RuntimeCase(
                kind=board_kind,
                arch=None,
                board=runtime_config.stem.removeprefix("board-"),
                case=case,
                case_dir=runtime_config.parent,
                wrapper_dir=wrapper_dir,
                runtime_config=runtime_config,
                build_config=build_config,
            )
        )
    return cases


def _matching_runtime_cases(
    cases: Sequence[_RuntimeCase], path: Path
) -> list[_RuntimeCase]:
    exact = [
        runtime_case
        for runtime_case in cases
        if path in {runtime_case.runtime_config, runtime_case.build_config}
    ]
    if exact:
        return exact

    case_matches = [
        runtime_case
        for runtime_case in cases
        if _is_prefix(path, runtime_case.case_dir)
    ]
    if case_matches:
        deepest = max(len(runtime_case.case_dir.parts) for runtime_case in case_matches)
        return [
            runtime_case
            for runtime_case in case_matches
            if len(runtime_case.case_dir.parts) == deepest
        ]

    wrapper_matches = [
        runtime_case
        for runtime_case in cases
        if _is_prefix(path, runtime_case.wrapper_dir)
    ]
    if wrapper_matches:
        deepest = max(
            len(runtime_case.wrapper_dir.parts) for runtime_case in wrapper_matches
        )
        return [
            runtime_case
            for runtime_case in wrapper_matches
            if len(runtime_case.wrapper_dir.parts) == deepest
        ]
    return []


def _starry_grouped_subcase(
    root: Path,
    path: Path,
    cases: Sequence[_RuntimeCase],
) -> tuple[list[_RuntimeCase], str] | None:
    system_root = root / "qemu/system"
    if not _is_prefix(path, system_root):
        return None
    relative = path.relative_to(system_root)
    if len(relative.parts) < 2:
        return None
    subcase = relative.parts[0]
    if not (system_root / subcase).is_dir():
        return None
    matching = [
        runtime_case
        for runtime_case in cases
        if runtime_case.kind == "starry-qemu" and runtime_case.case == "qemu/system"
    ]
    return matching, f"qemu/{subcase}"


def _nearest_qemu_build_config(
    start: Path,
    root: Path,
    arch: str,
    variant: str | None,
) -> Path | None:
    suffix = f"-{variant}" if variant else ""
    filename = f"build-{ARCH_TARGETS[arch]}{suffix}.toml"
    for directory in (start, *start.parents):
        if directory == root.parent:
            break
        candidate = directory / filename
        if candidate.is_file():
            return candidate
        if directory == root:
            break
    return None


def _nearest_board_build_config(start: Path, root: Path) -> Path | None:
    for directory in (start, *start.parents):
        if directory == root.parent:
            break
        candidates = sorted(directory.glob("build-*.toml"))
        if len(candidates) == 1:
            return candidates[0]
        if len(candidates) > 1:
            return None
        if directory == root:
            break
    return None


def _registered_template(
    registrations: Sequence[tuple[dict[str, Any], dict[str, Any]]],
    *,
    kind: str,
    arch: str | None = None,
    board: str | None = None,
    case: str | None = None,
) -> dict[str, Any] | None:
    matches = []
    for check, registration in registrations:
        if registration["kind"] != kind:
            continue
        if arch is not None and registration.get("arch") != arch:
            continue
        if board is not None and registration.get("board") != board:
            continue
        registered_cases = registration.get("cases")
        if registered_cases and case not in registered_cases:
            continue
        matches.append(check)
    if len(matches) > 1:
        ids = ", ".join(sorted(check["id"] for check in matches))
        raise SuiteRouteError(
            f"suite capability kind={kind} arch={arch} board={board} case={case} "
            f"is registered by multiple checks: {ids}"
        )
    return matches[0] if matches else None


def _suite_registrations(
    checks: Sequence[dict[str, Any]],
) -> list[tuple[dict[str, Any], dict[str, Any]]]:
    return [
        (check, registration)
        for check in checks
        for registration in check.get("suite", ())
    ]


def _selection(
    template: dict[str, Any],
    platform: str,
    case: str,
    command: str,
    path: Path,
) -> SuiteSelection:
    row_id = _slugify(f"suite-{template['id']}-{case}")
    return SuiteSelection(
        template_id=template["id"],
        row_id=row_id,
        leaf_name=f"{platform} · {case}",
        command=command,
        source_path=path.as_posix(),
    )


def _platform_label(template: dict[str, Any]) -> str:
    return template["name"].split(" · ", maxsplit=1)[0]


def _kind_os(kind: str) -> str:
    return kind.split("-", maxsplit=1)[0]


def _case_variant(case: str) -> str | None:
    suffix = case.rsplit("-", maxsplit=1)[-1]
    return suffix if suffix in {"vmx", "svm"} else None


def _parse_qemu_config_name(stem: str) -> tuple[str, str | None] | None:
    if not stem.startswith("qemu-"):
        return None
    value = stem.removeprefix("qemu-")
    for arch in ARCH_TARGETS:
        if value == arch:
            return arch, None
        if value.startswith(f"{arch}-"):
            return arch, value.removeprefix(f"{arch}-")
    return None


def _arches_for_path(path: Path) -> set[str]:
    rendered = path.as_posix().casefold()
    arches = set()
    for arch, target in ARCH_TARGETS.items():
        if re.search(rf"(^|[^a-z0-9]){re.escape(arch)}([^a-z0-9]|$)", rendered):
            arches.add(arch)
        if target.casefold() in rendered:
            arches.add(arch)
    if "riscv64gc" in rendered:
        arches.add("riscv64")
    return arches


def _arch_sort_key(arch: str) -> int:
    return list(ARCH_TARGETS).index(arch)


def _slugify(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", value.casefold()).strip("-")


def _is_prefix(path: Path, prefix: Path) -> bool:
    return path == prefix or prefix in path.parents
