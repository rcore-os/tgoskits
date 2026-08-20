#!/usr/bin/env python3

import json
import re
import subprocess
from collections import defaultdict
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

ARCH_TARGETS = {
    "aarch64": "aarch64-unknown-none-softfloat",
    "x86_64": "x86_64-unknown-none",
    "riscv64": "riscv64gc-unknown-none-elf",
    "loongarch64": "loongarch64-unknown-none-softfloat",
}
OS_ROOT_PACKAGES = {
    "arceos": "arceos-test-suit",
    "starry": "starryos",
    "axvisor": "axvisor",
}
ARCEOS_KTEST_EXCLUDED_PACKAGES = {"starry-kernel", "axvisor"}

FULL_PATHS = {
    Path("Cargo.toml"),
    Path("Cargo.lock"),
    Path("rust-toolchain"),
    Path("rust-toolchain.toml"),
}
FULL_PREFIXES = (
    Path(".cargo"),
    Path(".github/ci"),
    Path(".github/workflows"),
    Path("scripts/axbuild"),
    Path("scripts/test"),
    Path("xtask"),
    Path("os/StarryOS/starryos/xtask"),
    Path("os/axvisor/xtask"),
)
IGNORED_APP_PREFIX = Path("apps")
TEST_SUITE_PATHS = (
    (Path("test-suit/arceos"), "arceos"),
    (Path("test-suit/starryos"), "starry"),
    (Path("test-suit/axvisor"), "axvisor"),
)
KNOWN_OS_CONFIG_PATHS = (
    (Path("os/arceos/configs"), "arceos"),
    (Path("os/StarryOS/configs"), "starry"),
    (Path("os/axvisor/configs"), "axvisor"),
)
CI_OWNED_APP_INPUTS = ((Path("apps/arceos/virtio-blk-test"), "axvisor:qemu:aarch64"),)
ARCH_PATH_ALIASES = {
    "aarch64": (
        "a1000",
        "orangepi-5-plus",
        "phytiumpi",
        "rdk-s100",
        "rk3568",
        "rk3588",
        "roc-rk3568-pc",
        "rock-4d",
        "tac-e400",
    ),
    "x86_64": ("asus-nuc15crh",),
    "riscv64": (
        "aka-00-sg2002",
        "k230",
        "licheerv-nano-sg2002",
        "sg2002",
        "visionfive2",
    ),
    "loongarch64": ("jl-lsgd2k10", "ls2k1000"),
}


class ImpactError(ValueError):
    """Raised when a PR impact cannot be classified without losing coverage."""


@dataclass(frozen=True)
class CiImpact:
    full: bool
    reason: str
    changed_paths: tuple[str, ...]
    ignored_markdown: tuple[str, ...] = ()
    ignored_apps: tuple[str, ...] = ()
    changed_packages: tuple[str, ...] = ()
    affected_packages: tuple[str, ...] = ()
    affected_oses: tuple[str, ...] = ()
    input_selections: tuple[str, ...] = ()
    test_suite_paths: tuple[str, ...] = ()
    exclusive: bool = False
    targets: tuple[str, ...] = ()

    @classmethod
    def full_selection(
        cls,
        reason: str,
        changed_paths: Iterable[Path | str] = (),
        *,
        ignored_markdown: Iterable[str] = (),
        ignored_apps: Iterable[str] = (),
    ) -> "CiImpact":
        return cls(
            full=True,
            reason=reason,
            changed_paths=_sorted_path_strings(changed_paths),
            ignored_markdown=tuple(sorted(set(ignored_markdown))),
            ignored_apps=tuple(sorted(set(ignored_apps))),
            affected_oses=tuple(OS_ROOT_PACKAGES),
            targets=tuple(
                f"{os_name}:{arch}"
                for os_name in OS_ROOT_PACKAGES
                for arch in ARCH_TARGETS
            ),
        )


@dataclass(frozen=True)
class PackagePathEntry:
    package_id: str
    name: str
    relative_dir: Path


def analyze_pull_request(workspace_root: Path, since_ref: str) -> CiImpact:
    """Analyze one PR diff, falling back to the full matrix on uncertainty."""
    try:
        changed_paths = changed_paths_since(workspace_root, since_ref)
        if not changed_paths:
            return CiImpact.full_selection("the pull request diff is empty")
        ignored_markdown, ignored_apps, relevant = _partition_ignored(changed_paths)
        if not relevant:
            return CiImpact(
                full=False,
                reason="only ignored Markdown or app paths changed",
                changed_paths=_sorted_path_strings(changed_paths),
                ignored_markdown=tuple(sorted(ignored_markdown)),
                ignored_apps=tuple(sorted(ignored_apps)),
            )
        full_reason = _pre_metadata_full_reason(workspace_root, relevant)
        if full_reason is not None:
            return CiImpact.full_selection(
                full_reason,
                changed_paths,
                ignored_markdown=ignored_markdown,
                ignored_apps=ignored_apps,
            )
        metadata_by_arch = (
            {}
            if all(_is_known_input_path(path) for path in relevant)
            else load_metadata_by_arch(workspace_root)
        )
        return analyze_changed_paths(workspace_root, changed_paths, metadata_by_arch)
    except (
        ImpactError,
        OSError,
        UnicodeError,
        subprocess.SubprocessError,
        json.JSONDecodeError,
    ) as error:
        return CiImpact.full_selection(
            f"impact analysis failed: {error}",
            locals().get("changed_paths", ()),
            ignored_markdown=locals().get("ignored_markdown", ()),
            ignored_apps=locals().get("ignored_apps", ()),
        )


def changed_paths_since(workspace_root: Path, since_ref: str) -> list[Path]:
    if not since_ref.strip():
        raise ImpactError("the incremental base revision is empty")
    command = [
        "git",
        "diff",
        "--name-only",
        "--no-renames",
        "-z",
        f"{since_ref}...HEAD",
        "--",
    ]
    result = subprocess.run(
        command,
        cwd=workspace_root,
        check=True,
        capture_output=True,
    )
    return [
        _normalize_relative_path(Path(raw.decode("utf-8")))
        for raw in result.stdout.split(b"\0")
        if raw
    ]


def load_metadata_by_arch(workspace_root: Path) -> dict[str, dict]:
    metadata_by_arch = {}
    for arch, target in ARCH_TARGETS.items():
        command = [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--all-features",
            "--filter-platform",
            target,
            "--manifest-path",
            str(workspace_root / "Cargo.toml"),
        ]
        result = subprocess.run(
            command,
            cwd=workspace_root,
            check=True,
            text=True,
            capture_output=True,
        )
        metadata_by_arch[arch] = json.loads(result.stdout)
    return metadata_by_arch


def analyze_changed_paths(
    workspace_root: Path,
    changed_paths: Iterable[Path],
    metadata_by_arch: Mapping[str, dict],
) -> CiImpact:
    """Classify already-resolved changed paths using target-specific metadata."""
    workspace_root = workspace_root.resolve()
    paths = sorted({_normalize_relative_path(path) for path in changed_paths})
    ignored_markdown, ignored_apps, relevant_paths = _partition_ignored(paths)

    full_reason = _pre_metadata_full_reason(workspace_root, relevant_paths)
    if full_reason is not None:
        return CiImpact.full_selection(
            full_reason,
            paths,
            ignored_markdown=ignored_markdown,
            ignored_apps=ignored_apps,
        )

    try:
        changed_package_ids: set[str] = set()
        changed_package_names: set[str] = set()
        input_selections: set[str] = set()
        test_suite_paths: set[str] = set()
        package_paths: list[Path] = []
        unknown_paths: list[Path] = []

        for path in relevant_paths:
            if _test_suite_os(path) is not None:
                test_suite_paths.add(path.as_posix())
                continue
            known_inputs = _known_input_selections(path)
            if known_inputs:
                input_selections.update(known_inputs)
                continue
            package_paths.append(path)

        package_index = (
            _package_path_index(workspace_root, metadata_by_arch)
            if package_paths
            else []
        )
        for path in package_paths:
            package = _package_for_path(path, package_index)
            if package is not None:
                changed_package_ids.add(package.package_id)
                changed_package_names.add(package.name)
                continue
            unknown_paths.append(path)

        if unknown_paths:
            unknown = ", ".join(path.as_posix() for path in unknown_paths)
            return CiImpact.full_selection(
                f"changed path is outside known packages and OS inputs: {unknown}",
                paths,
                ignored_markdown=ignored_markdown,
                ignored_apps=ignored_apps,
            )

        targets = {
            target
            for selection in input_selections
            for target in _input_selection_targets(selection)
        }
        affected_names = set(changed_package_names)
        affected_oses: set[str] = set()
        for arch in ARCH_TARGETS if changed_package_ids else ():
            metadata = metadata_by_arch.get(arch)
            if metadata is None:
                raise ImpactError(f"missing cargo metadata for {arch}")
            affected_ids, id_to_name = _affected_package_ids(
                metadata, changed_package_ids
            )
            affected_names.update(
                id_to_name[package_id]
                for package_id in affected_ids
                if package_id in id_to_name
            )
            root_ids = _os_root_package_ids(metadata)
            for os_name, root_id in root_ids.items():
                if root_id in affected_ids:
                    affected_oses.add(os_name)
            if _affected_axtest_runs_in_arceos_ci(metadata, affected_ids, arch):
                targets.add(f"arceos:{arch}")

        for os_name in affected_oses:
            targets.update(f"{os_name}:{arch}" for arch in ARCH_TARGETS)

        return CiImpact(
            full=False,
            reason="pull request impact resolved",
            changed_paths=_sorted_path_strings(paths),
            ignored_markdown=tuple(sorted(ignored_markdown)),
            ignored_apps=tuple(sorted(ignored_apps)),
            changed_packages=tuple(sorted(changed_package_names)),
            affected_packages=tuple(sorted(affected_names)),
            affected_oses=tuple(
                os_name for os_name in OS_ROOT_PACKAGES if os_name in affected_oses
            ),
            input_selections=tuple(sorted(input_selections)),
            test_suite_paths=tuple(sorted(test_suite_paths)),
            exclusive=bool(test_suite_paths)
            and len(test_suite_paths) == len(relevant_paths),
            targets=tuple(sorted(targets, key=_target_sort_key)),
        )
    except (KeyError, TypeError, ValueError) as error:
        return CiImpact.full_selection(
            f"invalid cargo metadata: {error}",
            paths,
            ignored_markdown=ignored_markdown,
            ignored_apps=ignored_apps,
        )


def render_summary(
    impact: CiImpact,
    selected_check_ids: Sequence[str],
    skipped_check_ids: Sequence[str],
) -> str:
    mode = "full fallback" if impact.full else "incremental"
    lines = [
        "## PR CI impact",
        "",
        f"- Mode: **{mode}**",
        f"- Reason: {impact.reason}",
        f"- Changed paths: {_render_values(impact.changed_paths)}",
        f"- Ignored Markdown: {_render_values(impact.ignored_markdown)}",
        f"- Ignored apps: {_render_values(impact.ignored_apps)}",
        f"- Changed packages: {_render_values(impact.changed_packages)}",
        f"- Affected packages: {_render_values(impact.affected_packages)}",
        f"- Affected OSes: {_render_values(impact.affected_oses)}",
        f"- Precise inputs: {_render_values(impact.input_selections)}",
        f"- Test suites: {_render_values(impact.test_suite_paths)}",
        f"- Exclusive selection: `{str(impact.exclusive).lower()}`",
        f"- OS/arch targets: {_render_values(impact.targets)}",
        f"- Selected checks ({len(selected_check_ids)}): {_render_values(selected_check_ids)}",
        f"- Skipped checks ({len(skipped_check_ids)}): {_render_values(skipped_check_ids)}",
        "",
    ]
    return "\n".join(lines)


def _partition_ignored(
    changed_paths: Iterable[Path],
) -> tuple[set[str], set[str], list[Path]]:
    ignored_markdown = set()
    ignored_apps = set()
    relevant = []
    for path in changed_paths:
        path = _normalize_relative_path(path)
        rendered = path.as_posix()
        if path.suffix.casefold() == ".md":
            ignored_markdown.add(rendered)
        elif _is_prefix(path, IGNORED_APP_PREFIX) and not any(
            _is_prefix(path, prefix) for prefix, _ in CI_OWNED_APP_INPUTS
        ):
            ignored_apps.add(rendered)
        else:
            relevant.append(path)
    return ignored_markdown, ignored_apps, relevant


def _requires_full_matrix(path: Path) -> bool:
    return path in FULL_PATHS or any(
        _is_prefix(path, prefix) for prefix in FULL_PREFIXES
    )


def _pre_metadata_full_reason(
    workspace_root: Path, changed_paths: Iterable[Path]
) -> str | None:
    for path in changed_paths:
        if _requires_full_matrix(path):
            return f"global CI input `{path.as_posix()}` changed"
        if path.name == "Cargo.toml" and not (workspace_root / path).is_file():
            return f"package manifest `{path.as_posix()}` was deleted"
    return None


def _package_path_index(
    workspace_root: Path, metadata_by_arch: Mapping[str, dict]
) -> list[PackagePathEntry]:
    if not metadata_by_arch:
        raise ImpactError("cargo metadata is empty")
    first_metadata = next(iter(metadata_by_arch.values()))
    workspace_members = set(first_metadata["workspace_members"])
    entries = []
    for package in first_metadata["packages"]:
        if package["id"] not in workspace_members:
            continue
        manifest_path = Path(package["manifest_path"]).resolve()
        try:
            relative_dir = manifest_path.parent.relative_to(workspace_root)
        except ValueError as error:
            raise ImpactError(
                f"workspace package `{package['name']}` is outside the workspace"
            ) from error
        entries.append(PackagePathEntry(package["id"], package["name"], relative_dir))
    entries.sort(
        key=lambda entry: (-len(entry.relative_dir.parts), entry.name, entry.package_id)
    )
    return entries


def _package_for_path(
    path: Path, package_index: Sequence[PackagePathEntry]
) -> PackagePathEntry | None:
    for package in package_index:
        if path == package.relative_dir or _is_prefix(path, package.relative_dir):
            return package
    return None


def _affected_package_ids(
    metadata: dict, changed_package_ids: set[str]
) -> tuple[set[str], dict[str, str]]:
    workspace_members = set(metadata["workspace_members"])
    id_to_name = {
        package["id"]: package["name"]
        for package in metadata["packages"]
        if package["id"] in workspace_members
    }
    missing_changed = changed_package_ids - workspace_members
    if missing_changed:
        raise ImpactError(
            f"changed packages are missing from target metadata: {sorted(missing_changed)}"
        )
    resolve = metadata.get("resolve")
    if not isinstance(resolve, dict) or not isinstance(resolve.get("nodes"), list):
        raise ImpactError("cargo metadata did not provide a dependency graph")

    reverse_dependencies: dict[str, set[str]] = defaultdict(set)
    for node in resolve["nodes"]:
        node_id = node["id"]
        if node_id not in workspace_members:
            continue
        for dependency in node.get("deps", []):
            dependency_id = dependency["pkg"]
            if dependency_id in workspace_members:
                reverse_dependencies[dependency_id].add(node_id)

    affected = set()
    stack = list(changed_package_ids)
    while stack:
        package_id = stack.pop()
        if package_id in affected:
            continue
        affected.add(package_id)
        stack.extend(reverse_dependencies.get(package_id, ()))
    return affected, id_to_name


def _os_root_package_ids(metadata: dict) -> dict[str, str]:
    workspace_members = set(metadata["workspace_members"])
    names_to_ids: dict[str, list[str]] = defaultdict(list)
    for package in metadata["packages"]:
        if package["id"] in workspace_members:
            names_to_ids[package["name"]].append(package["id"])

    roots = {}
    for os_name, package_name in OS_ROOT_PACKAGES.items():
        ids = names_to_ids.get(package_name, [])
        if len(ids) != 1:
            raise ImpactError(
                f"expected exactly one workspace package `{package_name}`, found {len(ids)}"
            )
        roots[os_name] = ids[0]
    return roots


def _affected_axtest_runs_in_arceos_ci(
    metadata: dict, affected_ids: set[str], arch: str
) -> bool:
    workspace_members = set(metadata["workspace_members"])
    packages_by_id = {
        package["id"]: package
        for package in metadata["packages"]
        if package["id"] in workspace_members
    }
    axtest_ids = [
        package_id
        for package_id, package in packages_by_id.items()
        if package["name"] == "axtest"
    ]
    if not axtest_ids:
        return False
    if len(axtest_ids) != 1:
        raise ImpactError(
            f"expected exactly one workspace package `axtest`, found {len(axtest_ids)}"
        )
    axtest_id = axtest_ids[0]
    nodes_by_id = {node["id"]: node for node in metadata["resolve"]["nodes"]}

    for package_id in affected_ids:
        package = packages_by_id[package_id]
        if package["name"] in ARCEOS_KTEST_EXCLUDED_PACKAGES:
            continue
        node = nodes_by_id.get(package_id)
        if node is None:
            raise ImpactError(f"missing dependency node for `{package['name']}`")
        uses_axtest = any(
            dependency["pkg"] == axtest_id
            and any(
                dependency_kind.get("kind") == "dev"
                for dependency_kind in dependency.get("dep_kinds", [])
            )
            for dependency in node.get("deps", [])
        )
        if not uses_axtest:
            continue

        package_metadata = package.get("metadata") or {}
        if not isinstance(package_metadata, dict):
            raise ImpactError(f"invalid package metadata for `{package['name']}`")
        axtest_metadata = package_metadata.get("axtest") or {}
        if not isinstance(axtest_metadata, dict):
            raise ImpactError(f"invalid axtest metadata for `{package['name']}`")
        runtime = axtest_metadata.get("runtime", "arceos")
        if runtime not in {"arceos", "starry", "axvisor", "board"}:
            raise ImpactError(
                f"unsupported axtest runtime `{runtime}` for `{package['name']}`"
            )
        if runtime == "board":
            continue
        if arch in _package_axtest_arches(package):
            return True
    return False


def _package_axtest_arches(package: dict) -> set[str]:
    package_metadata = package.get("metadata") or {}
    docs_metadata = package_metadata.get("docs") or {}
    if not isinstance(docs_metadata, dict):
        raise ImpactError(f"invalid docs metadata for `{package['name']}`")
    rustdoc_metadata = docs_metadata.get("rs") or {}
    if not isinstance(rustdoc_metadata, dict):
        raise ImpactError(f"invalid docs.rs metadata for `{package['name']}`")
    declared_targets = rustdoc_metadata.get("targets")
    if declared_targets is None:
        return {"x86_64"}
    if not isinstance(declared_targets, list) or any(
        not isinstance(target, str) for target in declared_targets
    ):
        raise ImpactError(f"invalid docs.rs targets for `{package['name']}`")
    target_to_arch = {target: arch for arch, target in ARCH_TARGETS.items()}
    supported_arches = {
        target_to_arch[target]
        for target in declared_targets
        if target in target_to_arch
    }
    if not supported_arches:
        raise ImpactError(
            f"package `{package['name']}` has no supported bare-metal axtest target"
        )
    return supported_arches


def _is_known_input_path(path: Path) -> bool:
    return _test_suite_os(path) is not None or bool(_known_input_selections(path))


def _test_suite_os(path: Path) -> str | None:
    for prefix, os_name in TEST_SUITE_PATHS:
        if _is_prefix(path, prefix):
            return os_name
    return None


def _known_input_selections(path: Path) -> set[str]:
    for prefix, selection in CI_OWNED_APP_INPUTS:
        if _is_prefix(path, prefix):
            return {selection}

    for prefix, os_name in KNOWN_OS_CONFIG_PATHS:
        if not _is_prefix(path, prefix):
            continue
        arches = _arch_hints(path)
        relative = path.relative_to(prefix)
        parts = tuple(part.casefold() for part in relative.parts)
        if "qemu" in parts and arches:
            return {f"{os_name}:qemu:{arch}" for arch in arches}
        if "board" in parts and path.suffix == ".toml":
            return {f"{os_name}:board:{path.stem.casefold()}"}
        return {f"{os_name}:all"}
    return set()


def _input_selection_targets(selection: str) -> set[str]:
    os_name, _, detail = selection.partition(":")
    if os_name not in OS_ROOT_PACKAGES:
        return set()
    _, _, value = detail.partition(":")
    if value in ARCH_TARGETS:
        return {f"{os_name}:{value}"}
    arches = _arch_hints(Path(value))
    if arches:
        return {f"{os_name}:{arch}" for arch in arches}
    return {f"{os_name}:{arch}" for arch in ARCH_TARGETS}


def _arch_hints(path: Path) -> set[str]:
    rendered = path.as_posix().casefold()
    hints = set()
    for arch, target in ARCH_TARGETS.items():
        aliases = {arch.casefold(), target.casefold()}
        if any(
            re.search(rf"(^|[^a-z0-9]){re.escape(alias)}([^a-z0-9]|$)", rendered)
            for alias in aliases
        ):
            hints.add(arch)
    if "riscv64gc" in rendered:
        hints.add("riscv64")
    for arch, aliases in ARCH_PATH_ALIASES.items():
        if any(alias in rendered for alias in aliases):
            hints.add(arch)
    return hints


def _normalize_relative_path(path: Path) -> Path:
    if path.is_absolute():
        raise ImpactError(f"absolute changed path is not allowed: {path}")
    normalized = Path()
    for component in path.parts:
        if component in ("", "."):
            continue
        if component == "..":
            raise ImpactError(f"parent traversal is not allowed: {path}")
        normalized /= component
    if not normalized.parts:
        raise ImpactError("empty changed path is not allowed")
    return normalized


def _is_prefix(path: Path, prefix: Path) -> bool:
    return path == prefix or prefix in path.parents


def _target_sort_key(target: str) -> tuple[int, int, str]:
    os_name, _, arch = target.partition(":")
    return (
        list(OS_ROOT_PACKAGES).index(os_name) if os_name in OS_ROOT_PACKAGES else 999,
        list(ARCH_TARGETS).index(arch) if arch in ARCH_TARGETS else 999,
        target,
    )


def _sorted_path_strings(paths: Iterable[Path | str]) -> tuple[str, ...]:
    return tuple(sorted({Path(path).as_posix() for path in paths}))


def _render_values(values: Sequence[str]) -> str:
    if not values:
        return "none"
    return ", ".join(
        f"`{value.replace('`', '').replace(chr(10), ' ')}`" for value in values
    )
