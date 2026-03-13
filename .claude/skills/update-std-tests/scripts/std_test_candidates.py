#!/usr/bin/env python3

import argparse
import csv
import json
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable

STD_CRATES_CSV = Path("scripts/test/std_crates.csv")

EXCLUDED_PACKAGE_NAMES = {
    "tg-xtask": "tooling package",
    "arm_vcpu": "host-incompatible architecture-specific package",
    "riscv_vcpu": "host-incompatible architecture-specific package",
    "axvisor": "bare-metal application package",
}

EXCLUDED_FAILURE_PATTERNS = {
    "invalid register": "host-incompatible inline assembly",
    "undefined symbol: main": "missing host entrypoint",
}

FAILURE_PATTERNS = [
    "invalid register",
    "undefined symbol: main",
    "linking with `cc` failed",
    "could not compile",
    "error:",
]


@dataclass
class Candidate:
    name: str
    target_kind: str
    manifest_path: str
    reason_or_failure: str


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    try:
        return args.handler(args)
    except Exception as exc:  # pragma: no cover - exercised via CLI
        print(f"error: {exc}", file=sys.stderr)
        return 1


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Audit and update scripts/test/std_crates.csv candidates."
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    audit = subparsers.add_parser(
        "audit", help="Classify workspace packages against std_crates.csv"
    )
    audit.add_argument(
        "--repo-root",
        default=".",
        help="Path to the repository root (default: current directory)",
    )
    audit.add_argument(
        "--format",
        choices=["json", "markdown"],
        default="markdown",
        help="Output format",
    )
    audit.set_defaults(handler=handle_audit)

    apply_cmd = subparsers.add_parser(
        "apply", help="Add confirmed packages to scripts/test/std_crates.csv"
    )
    apply_cmd.add_argument(
        "--repo-root",
        default=".",
        help="Path to the repository root (default: current directory)",
    )
    apply_cmd.add_argument(
        "--packages",
        nargs="+",
        required=True,
        help="Workspace packages to merge into std_crates.csv",
    )
    apply_cmd.set_defaults(handler=handle_apply)
    return parser


def handle_audit(args: argparse.Namespace) -> int:
    repo_root = Path(args.repo_root).resolve()
    audit = build_audit(repo_root)
    if args.format == "json":
        print(json.dumps(audit, indent=2, ensure_ascii=False))
    else:
        print(render_markdown(audit))
    return 0


def handle_apply(args: argparse.Namespace) -> int:
    repo_root = Path(args.repo_root).resolve()
    metadata = load_metadata(repo_root)
    workspace_packages = workspace_package_info(metadata, repo_root)
    workspace_names = [package["name"] for package in workspace_packages]
    workspace_name_set = set(workspace_names)

    csv_path = repo_root / STD_CRATES_CSV
    existing_csv_packages = load_csv_packages(csv_path, workspace_name_set)
    requested = dedupe(args.packages)

    unknown = [name for name in requested if name not in workspace_name_set]
    if unknown:
        raise ValueError(f"unknown workspace packages: {', '.join(unknown)}")

    merged = set(existing_csv_packages)
    merged.update(requested)
    ordered = [name for name in workspace_names if name in merged]
    write_csv_packages(csv_path, ordered)

    added = [name for name in requested if name not in existing_csv_packages]
    if added:
        print(f"added {len(added)} package(s): {', '.join(added)}")
    else:
        print("no new packages were added")
    print(f"wrote {len(ordered)} package(s) to {csv_path}")
    return 0


def build_audit(repo_root: Path) -> dict:
    metadata = load_metadata(repo_root)
    workspace_packages = workspace_package_info(metadata, repo_root)
    workspace_names = {package["name"] for package in workspace_packages}
    csv_path = repo_root / STD_CRATES_CSV
    existing_csv_packages = load_csv_packages(csv_path, workspace_names)

    passing_candidates: list[Candidate] = []
    failing_candidates: list[Candidate] = []
    excluded_candidates: list[Candidate] = []

    for package in workspace_packages:
        name = package["name"]
        if name in existing_csv_packages:
            continue

        target_kind = package["target_kind"]
        manifest_path = package["manifest_path"]

        if name in EXCLUDED_PACKAGE_NAMES:
            excluded_candidates.append(
                Candidate(name, target_kind, manifest_path, EXCLUDED_PACKAGE_NAMES[name])
            )
            continue

        if target_kind == "staticlib-only":
            excluded_candidates.append(
                Candidate(name, target_kind, manifest_path, "staticlib-only package")
            )
            continue

        if target_kind not in {"lib", "bin-only"}:
            excluded_candidates.append(
                Candidate(
                    name,
                    target_kind,
                    manifest_path,
                    f"unsupported target kind: {target_kind}",
                )
            )
            continue

        success, failure = cargo_test_package(repo_root, name)
        if success:
            passing_candidates.append(
                Candidate(name, target_kind, manifest_path, "passes cargo test")
            )
            continue

        excluded_reason = excluded_failure_reason(failure)
        if excluded_reason is not None:
            excluded_candidates.append(
                Candidate(name, target_kind, manifest_path, excluded_reason)
            )
            continue

        failing_candidates.append(Candidate(name, target_kind, manifest_path, failure))

    return {
        "repo_root": str(repo_root),
        "csv_path": str(csv_path),
        "existing_csv_packages": existing_csv_packages,
        "passing_candidates": [asdict(candidate) for candidate in passing_candidates],
        "failing_candidates": [asdict(candidate) for candidate in failing_candidates],
        "excluded_candidates": [asdict(candidate) for candidate in excluded_candidates],
    }


def render_markdown(audit: dict) -> str:
    sections = [
        "# Std Test Audit",
        "",
        f"- CSV path: `{audit['csv_path']}`",
        f"- Existing CSV packages: {len(audit['existing_csv_packages'])}",
        "",
    ]
    sections.extend(
        render_candidate_section(
            "Passing candidates",
            audit["passing_candidates"],
            "No passing candidates were found.",
        )
    )
    sections.extend(
        render_candidate_section(
            "Failing candidates",
            audit["failing_candidates"],
            "No failing candidates were found.",
        )
    )
    sections.extend(
        render_candidate_section(
            "Excluded candidates",
            audit["excluded_candidates"],
            "No excluded candidates were found.",
        )
    )
    return "\n".join(sections).rstrip()


def render_candidate_section(
    title: str, candidates: list[dict], empty_message: str
) -> list[str]:
    lines = [f"## {title}", ""]
    if not candidates:
        lines.append(empty_message)
        lines.append("")
        return lines

    for candidate in candidates:
        lines.append(
            "- `{name}` ({target_kind}) - `{manifest_path}` - {reason_or_failure}".format(
                **candidate
            )
        )
    lines.append("")
    return lines


def load_metadata(repo_root: Path) -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "cargo metadata failed")
    return json.loads(result.stdout)


def workspace_package_info(metadata: dict, repo_root: Path) -> list[dict]:
    packages_by_id = {package["id"]: package for package in metadata["packages"]}
    package_info = []
    for package_id in metadata["workspace_members"]:
        package = packages_by_id[package_id]
        manifest_path = display_manifest_path(Path(package["manifest_path"]), repo_root)
        package_info.append(
            {
                "name": package["name"],
                "manifest_path": manifest_path,
                "target_kind": classify_target_kind(package),
            }
        )
    return package_info


def display_manifest_path(manifest_path: Path, repo_root: Path) -> str:
    for base in (repo_root, repo_root.resolve()):
        try:
            return str(manifest_path.relative_to(base))
        except ValueError:
            continue
    return str(manifest_path)


def classify_target_kind(package: dict) -> str:
    target_kinds = {
        kind
        for target in package["targets"]
        for kind in target["kind"]
        if kind not in {"custom-build", "bench", "test", "example"}
    }

    if "lib" in target_kinds:
        return "lib"
    if "bin" in target_kinds and "staticlib" not in target_kinds:
        return "bin-only"
    if "staticlib" in target_kinds and "lib" not in target_kinds and "bin" not in target_kinds:
        return "staticlib-only"
    if not target_kinds:
        return "unknown"
    return "+".join(sorted(target_kinds))


def cargo_test_package(repo_root: Path, package: str) -> tuple[bool, str]:
    result = subprocess.run(
        ["cargo", "test", "-p", package],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0:
        return True, ""
    return False, extract_failure_excerpt(result.stdout, result.stderr)


def extract_failure_excerpt(stdout: str, stderr: str) -> str:
    lines = [line.strip() for line in (stderr + "\n" + stdout).splitlines() if line.strip()]
    lowered = [line.lower() for line in lines]
    for pattern in FAILURE_PATTERNS:
        for index, line in enumerate(lowered):
            if pattern in line:
                return lines[index]
    return lines[-1] if lines else "cargo test failed"


def excluded_failure_reason(failure: str) -> str | None:
    lowered = failure.lower()
    for pattern, reason in EXCLUDED_FAILURE_PATTERNS.items():
        if pattern in lowered:
            return reason
    return None


def load_csv_packages(csv_path: Path, workspace_names: set[str]) -> list[str]:
    if not csv_path.exists():
        raise FileNotFoundError(f"missing csv file: {csv_path}")

    with csv_path.open(newline="", encoding="utf-8") as handle:
        reader = csv.reader(handle)
        rows = [
            [cell.strip() for cell in row]
            for row in reader
            if row and any(cell.strip() for cell in row)
        ]

    if not rows:
        raise ValueError(f"csv file is empty: {csv_path}")

    header = rows[0][0].lstrip("\ufeff")
    if header != "package":
        raise ValueError(f"invalid csv header in {csv_path}: expected `package`")

    packages: list[str] = []
    seen = set()
    for row in rows[1:]:
        package = row[0]
        if package in seen:
            raise ValueError(f"duplicate package in {csv_path}: {package}")
        if package not in workspace_names:
            raise ValueError(f"unknown workspace package in {csv_path}: {package}")
        packages.append(package)
        seen.add(package)
    return packages


def write_csv_packages(csv_path: Path, packages: Iterable[str]) -> None:
    with csv_path.open("w", newline="", encoding="utf-8") as handle:
        handle.write("package\n")
        for package in packages:
            handle.write(f"{package}\n")


def dedupe(items: Iterable[str]) -> list[str]:
    seen = set()
    ordered = []
    for item in items:
        if item in seen:
            continue
        seen.add(item)
        ordered.append(item)
    return ordered


if __name__ == "__main__":
    raise SystemExit(main())
