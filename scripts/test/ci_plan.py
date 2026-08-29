#!/usr/bin/env python3

import argparse
import json
import os
import re
import sys
from collections.abc import Iterable
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

if sys.version_info < (3, 11):
    raise SystemExit("ci_plan.py requires Python 3.11 or newer")

import tomllib
from ci_impact import ARCH_TARGETS, CiImpact, analyze_pull_request, render_summary
from ci_runner_profiles import (
    GLOBAL_DEFAULT_RUNNER,
    RunnerProfileError,
    load_runner_profiles,
)
from ci_suite import (
    SUITE_FIELDS,
    SUPPORTED_SUITE_KINDS,
    SuiteRouteError,
    SuiteSelection,
    check_matches_input,
    resolve_suite_selections,
    validate_suite_catalog,
)

WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
CHECKS_ROOT = WORKSPACE_ROOT / ".github" / "ci" / "checks"
MAIN_MANIFESTS = (
    CHECKS_ROOT / "static.toml",
    CHECKS_ROOT / "workspace.toml",
    CHECKS_ROOT / "arceos.toml",
    CHECKS_ROOT / "axvisor.toml",
    CHECKS_ROOT / "starry.toml",
)
STARRY_APPS_MANIFEST = CHECKS_ROOT / "starry-apps.toml"
RUNNER_PROFILES_MANIFEST = CHECKS_ROOT.parent / "runner-profiles.toml"

SUPPORTED_PHASES = {"static", "test", "starry_apps"}
SUPPORTED_PREFLIGHTS = {"none", "qemu-user", "full"}
TEST_GROUP_OUTPUT_PREFIXES = {
    "Workspace": "workspace",
    "ArceOS": "arceos",
    "Starry": "starry",
    "AxVisor": "axvisor",
}
SUPPORTED_IMPACT_TARGETS = {
    f"{os_name}:{arch}"
    for os_name in ("arceos", "starry", "axvisor")
    for arch in ("aarch64", "x86_64", "riscv64", "loongarch64")
}
SUPPORTED_IMPACT_PACKAGES = {"axloader"}
CHECK_ID_PATTERN = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
TOP_LEVEL_FIELDS = {
    "schema_version",
    "phase",
    "group",
    "default_runner",
    "check",
}
CHECK_FIELDS = {
    "id",
    "name",
    "runner",
    "command",
    "required_base_branch",
    "fetch_depth",
    "timeout_minutes",
    "cache_key",
    "wifi_secrets",
    "apk_region",
    "upload_xtask_bin_artifact",
    "download_xtask_bin_artifact",
    "xtask_bin_artifact_name",
    "container_preflight",
    "events",
    "enable_boolean_input",
    "impact_targets",
    "impact_packages",
    "pull_request_command",
    "suite",
}
REQUIRED_CHECK_FIELDS = {"id", "name", "command"}
BOOLEAN_CHECK_FIELDS = {
    "upload_xtask_bin_artifact",
    "download_xtask_bin_artifact",
    "wifi_secrets",
}


class PlanError(ValueError):
    """Raised when a CI check manifest violates the planner contract."""


@dataclass(frozen=True)
class PlanContext:
    repository: str
    repository_owner: str
    event_name: str
    base_ref: str = ""
    enabled_boolean_inputs: frozenset[str] = frozenset()
    impact: CiImpact | None = None


def load_catalog(manifests: Iterable[Path]) -> list[dict[str, Any]]:
    checks: list[dict[str, Any]] = []
    seen_ids: set[str] = set()
    runner_profiles = _load_runner_profiles(RUNNER_PROFILES_MANIFEST)

    for manifest in manifests:
        document = _load_manifest(manifest)
        phase = document["phase"]
        group = document["group"]
        default_runner = document.get("default_runner", GLOBAL_DEFAULT_RUNNER)
        if default_runner not in runner_profiles:
            raise PlanError(
                f"{manifest} references unknown default_runner '{default_runner}'"
            )
        for index, raw_check in enumerate(document["check"], start=1):
            location = f"{manifest}:{index}"
            check = _validate_check(
                raw_check,
                location,
                runner_profiles,
                default_runner,
            )
            check_id = check["id"]
            if check_id in seen_ids:
                raise PlanError(f"duplicate check id '{check_id}' at {location}")
            seen_ids.add(check_id)
            check["phase"] = phase
            check["group"] = group
            check["source"] = str(manifest.relative_to(WORKSPACE_ROOT))
            checks.append(check)

    _validate_artifact_contract(checks)
    _validate_impact_contract(checks)
    try:
        validate_suite_catalog(WORKSPACE_ROOT, checks)
    except SuiteRouteError as error:
        raise PlanError(str(error)) from error
    return checks


def build_main_plan(context: PlanContext) -> dict[str, Any]:
    checks = load_catalog(MAIN_MANIFESTS)
    context = _resolve_input_fallbacks(checks, context)
    return _build_main_plan(checks, context)


def _build_main_plan(
    checks: list[dict[str, Any]], context: PlanContext
) -> dict[str, Any]:
    suite_only = bool(
        context.event_name == "pull_request"
        and context.impact is not None
        and context.impact.exclusive
        and context.impact.test_suite_paths
    )
    static_rows = [] if suite_only else _plan_phase(checks, "static", context)
    test_rows = (
        _plan_suite_rows(checks, context)
        if suite_only
        else _plan_phase(checks, "test", context) + _plan_suite_rows(checks, context)
    )
    if not test_rows:
        raise PlanError("main CI must resolve to a non-empty test matrix")
    if not suite_only and not static_rows:
        raise PlanError("main CI must resolve to a non-empty static matrix")
    outputs = {
        "static_matrix": {"include": static_rows},
        "static_required": bool(static_rows),
    }
    outputs.update(_build_test_group_outputs(test_rows))
    return outputs


def _build_test_group_outputs(
    test_rows: list[dict[str, Any]],
) -> dict[str, Any]:
    rows_by_group = {group: [] for group in TEST_GROUP_OUTPUT_PREFIXES}
    for row in test_rows:
        group = row["group"]
        if group not in rows_by_group:
            raise PlanError(
                f"test row '{row['id']}' uses unsupported group '{group}'"
            )
        rows_by_group[group].append(row)

    outputs = {}
    for group, prefix in TEST_GROUP_OUTPUT_PREFIXES.items():
        rows = rows_by_group[group]
        outputs[f"{prefix}_matrix"] = {"include": rows}
        outputs[f"{prefix}_required"] = bool(rows)
    return outputs


def build_starry_apps_plan(
    context: PlanContext,
) -> dict[str, dict[str, list[dict[str, Any]]]]:
    checks = load_catalog((STARRY_APPS_MANIFEST,))
    rows = _plan_phase(checks, "starry_apps", context)
    if not rows:
        raise PlanError("Starry Apps must resolve to a non-empty matrix")
    return {"starry_apps_matrix": {"include": rows}}


def write_github_outputs(outputs: dict[str, Any], output_file: Path) -> None:
    with output_file.open("a", encoding="utf-8") as output:
        for name, value in outputs.items():
            encoded = json.dumps(value, separators=(",", ":"))
            output.write(f"{name}={encoded}\n")


def _load_manifest(manifest: Path) -> dict[str, Any]:
    if not manifest.is_file():
        raise PlanError(f"missing CI check manifest: {manifest}")
    with manifest.open("rb") as source:
        document = tomllib.load(source)

    unknown_fields = set(document) - TOP_LEVEL_FIELDS
    if unknown_fields:
        raise PlanError(
            f"{manifest} has unknown top-level fields: {sorted(unknown_fields)}"
        )
    if document.get("schema_version") != 3:
        raise PlanError(f"{manifest} must declare schema_version = 3")
    phase = document.get("phase")
    if phase not in SUPPORTED_PHASES:
        raise PlanError(f"{manifest} has an unsupported phase")
    group = document.get("group")
    if not isinstance(group, str) or not group.strip():
        raise PlanError(f"{manifest} must declare a non-empty group")
    if phase == "test" and group not in TEST_GROUP_OUTPUT_PREFIXES:
        raise PlanError(f"{manifest} has unsupported test group '{group}'")
    if not isinstance(document.get("check"), list) or not document["check"]:
        raise PlanError(f"{manifest} must contain at least one [[check]] entry")
    return document


def _validate_check(
    raw_check: Any,
    location: str,
    runner_profiles: dict[str, dict[str, Any]] | None = None,
    default_runner: str = GLOBAL_DEFAULT_RUNNER,
) -> dict[str, Any]:
    if not isinstance(raw_check, dict):
        raise PlanError(f"{location} check entry must be a table")
    unknown_fields = set(raw_check) - CHECK_FIELDS
    if unknown_fields:
        raise PlanError(f"{location} has unknown fields: {sorted(unknown_fields)}")
    missing_fields = REQUIRED_CHECK_FIELDS - set(raw_check)
    if missing_fields:
        raise PlanError(f"{location} is missing fields: {sorted(missing_fields)}")

    profiles = runner_profiles or _load_runner_profiles(RUNNER_PROFILES_MANIFEST)
    runner = raw_check.get("runner", default_runner)
    if not isinstance(runner, str) or not runner:
        raise PlanError(f"{location} field 'runner' must be a non-empty string")
    if runner not in profiles:
        raise PlanError(f"{location} references unknown runner profile '{runner}'")

    check = {key: value for key, value in raw_check.items() if key != "runner"}
    check.update(profiles[runner])
    check["runner"] = runner
    for field in ("id", "name", "command"):
        if not isinstance(check[field], str) or not check[field].strip():
            raise PlanError(f"{location} field '{field}' must be a non-empty string")
    check["name"] = check["name"].strip()
    if CHECK_ID_PATTERN.fullmatch(check["id"]) is None:
        raise PlanError(f"{location} field 'id' must use lowercase kebab-case")
    runs_on = check["runs_on"]
    required_base = check.get("required_base_branch")
    if required_base is not None and (
        not isinstance(required_base, str) or not required_base
    ):
        raise PlanError(
            f"{location} field 'required_base_branch' must be a non-empty string"
        )

    for field in BOOLEAN_CHECK_FIELDS:
        value = check.get(field)
        if value is not None and not isinstance(value, bool):
            raise PlanError(f"{location} field '{field}' must be boolean")

    timeout = check.get("timeout_minutes", 360)
    if not isinstance(timeout, int) or timeout <= 0:
        raise PlanError(f"{location} timeout_minutes must be positive")

    fetch_depth = str(check.get("fetch_depth", "1"))
    if (
        fetch_depth != "full"
        and re.fullmatch(r"0|[1-9][0-9]*", fetch_depth) is None
    ):
        raise PlanError(
            f"{location} fetch_depth must be 'full' or a non-negative integer"
        )

    preflight = check.get("container_preflight")
    if preflight is not None and preflight not in SUPPORTED_PREFLIGHTS:
        raise PlanError(f"{location} has an unsupported container_preflight")

    events = check.get("events")
    if events is not None and (
        not isinstance(events, list)
        or not events
        or any(not isinstance(event, str) or not event for event in events)
    ):
        raise PlanError(f"{location} events must be a non-empty string array")
    boolean_input = check.get("enable_boolean_input")
    if boolean_input is not None and (
        not isinstance(boolean_input, str) or not boolean_input
    ):
        raise PlanError(f"{location} enable_boolean_input must be a non-empty string")

    cache_key = check.get("cache_key", "")
    if not isinstance(cache_key, str):
        raise PlanError(f"{location} cache_key must be a string")
    if "self-hosted" in runs_on and cache_key:
        raise PlanError(f"{location} self-hosted checks must use an empty cache_key")

    artifact_name = check.get("xtask_bin_artifact_name")
    if artifact_name is not None and (
        not isinstance(artifact_name, str) or not artifact_name
    ):
        raise PlanError(
            f"{location} xtask_bin_artifact_name must be a non-empty string"
        )
    if check.get("upload_xtask_bin_artifact") and check.get(
        "download_xtask_bin_artifact"
    ):
        raise PlanError(f"{location} cannot both upload and download the artifact")

    impact_targets = check.get("impact_targets")
    if impact_targets is not None:
        _validate_string_array(impact_targets, "impact_targets", location)
        unsupported_targets = set(impact_targets) - SUPPORTED_IMPACT_TARGETS
        if unsupported_targets:
            raise PlanError(
                f"{location} has unsupported impact_targets: {sorted(unsupported_targets)}"
            )

    impact_packages = check.get("impact_packages")
    if impact_packages is not None:
        _validate_string_array(impact_packages, "impact_packages", location)
        unsupported_packages = set(impact_packages) - SUPPORTED_IMPACT_PACKAGES
        if unsupported_packages:
            raise PlanError(
                f"{location} has unsupported impact_packages: {sorted(unsupported_packages)}"
            )

    pull_request_command = check.get("pull_request_command")
    if pull_request_command is not None and (
        not isinstance(pull_request_command, str) or not pull_request_command.strip()
    ):
        raise PlanError(f"{location} pull_request_command must be a non-empty string")

    suite = check.get("suite")
    if suite is not None:
        _validate_suite_registrations(suite, location)

    return check


def _load_runner_profiles(manifest: Path) -> dict[str, dict[str, Any]]:
    try:
        return load_runner_profiles(manifest)
    except RunnerProfileError as error:
        raise PlanError(str(error)) from error


def _validate_suite_registrations(suite: Any, location: str) -> None:
    if not isinstance(suite, list) or not suite:
        raise PlanError(f"{location} suite must be a non-empty table array")
    for index, registration in enumerate(suite, start=1):
        suite_location = f"{location} suite {index}"
        if not isinstance(registration, dict):
            raise PlanError(f"{suite_location} must be a table")
        unknown_fields = set(registration) - SUITE_FIELDS
        if unknown_fields:
            raise PlanError(
                f"{suite_location} has unknown fields: {sorted(unknown_fields)}"
            )
        kind = registration.get("kind")
        if kind not in SUPPORTED_SUITE_KINDS:
            raise PlanError(f"{suite_location} has an unsupported kind")
        is_qemu = kind.endswith("-qemu")
        arch = registration.get("arch")
        board = registration.get("board")
        if is_qemu and arch not in ARCH_TARGETS:
            raise PlanError(f"{suite_location} must declare a supported arch")
        if not is_qemu and (not isinstance(board, str) or not board):
            raise PlanError(f"{suite_location} must declare a non-empty board")
        if is_qemu and board is not None:
            raise PlanError(f"{suite_location} qemu registration cannot declare board")
        if not is_qemu and arch is not None:
            raise PlanError(f"{suite_location} board registration cannot declare arch")
        cases = registration.get("cases")
        if cases is not None:
            _validate_string_array(cases, "cases", suite_location)


def _validate_string_array(value: Any, field: str, location: str) -> None:
    if (
        not isinstance(value, list)
        or not value
        or any(not isinstance(item, str) or not item for item in value)
        or len(set(value)) != len(value)
    ):
        raise PlanError(f"{location} {field} must be a unique non-empty string array")


def _validate_artifact_contract(checks: list[dict[str, Any]]) -> None:
    main_checks = [check for check in checks if check["phase"] in {"static", "test"}]
    if not main_checks:
        return

    producers = [
        check for check in main_checks if check.get("upload_xtask_bin_artifact", False)
    ]
    if len(producers) != 1 or producers[0]["phase"] != "static":
        raise PlanError(
            "main CI must define exactly one static xtask artifact producer"
        )

    producer_name = producers[0].get("xtask_bin_artifact_name", "tg-xtask-bin")
    for check in main_checks:
        if not check.get("download_xtask_bin_artifact", False):
            continue
        consumer_name = check.get("xtask_bin_artifact_name", "tg-xtask-bin")
        if consumer_name != producer_name:
            raise PlanError(
                f"check '{check['id']}' consumes unknown artifact '{consumer_name}'"
            )


def _validate_impact_contract(checks: list[dict[str, Any]]) -> None:
    for check in checks:
        if check["phase"] != "test" or check["group"] == "Workspace":
            continue
        if not check.get("impact_targets") and not check.get("impact_packages"):
            raise PlanError(
                f"test check '{check['id']}' must declare impact_targets or impact_packages"
            )


def _plan_phase(
    checks: list[dict[str, Any]], phase: str, context: PlanContext
) -> list[dict[str, Any]]:
    return [
        _normalize_check(check, context)
        for check in checks
        if check["phase"] == phase
        and _is_enabled(check, context)
        and _matches_impact(check, context)
    ]


def _resolve_input_fallbacks(
    checks: list[dict[str, Any]], context: PlanContext
) -> PlanContext:
    impact = context.impact
    if impact is None or impact.full or not impact.input_selections:
        return context

    resolved = []
    fallback_inputs = []
    for selection in impact.input_selections:
        if any(check_matches_input(check, selection) for check in checks):
            resolved.append(selection)
            continue
        os_name = selection.partition(":")[0]
        fallback = f"{os_name}:all"
        resolved.append(fallback)
        fallback_inputs.append(selection)
    if not fallback_inputs:
        return context

    reason = (
        f"{impact.reason}; unmatched precise inputs fell back to OS-wide: "
        f"{', '.join(fallback_inputs)}"
    )
    resolved_impact = replace(
        impact,
        reason=reason,
        input_selections=tuple(sorted(set(resolved))),
    )
    return replace(context, impact=resolved_impact)


def _plan_suite_rows(
    checks: list[dict[str, Any]], context: PlanContext
) -> list[dict[str, Any]]:
    impact = context.impact
    if (
        context.event_name != "pull_request"
        or impact is None
        or impact.full
        or not impact.test_suite_paths
    ):
        return []

    suite_paths = tuple(
        path
        for path in impact.test_suite_paths
        if _suite_path_os(path) not in impact.affected_oses
    )
    if not suite_paths:
        return []
    try:
        selections = resolve_suite_selections(WORKSPACE_ROOT, checks, suite_paths)
    except SuiteRouteError as error:
        raise PlanError(str(error)) from error

    checks_by_id = {check["id"]: check for check in checks}
    rows = []
    for selection in selections:
        template = checks_by_id[selection.template_id]
        if not _is_enabled(template, context):
            raise PlanError(
                f"test suite path `{selection.source_path}` requires unavailable "
                f"check '{selection.template_id}'"
            )
        rows.append(_normalize_suite_selection(template, selection, context))
    return rows


def _normalize_suite_selection(
    template: dict[str, Any],
    selection: SuiteSelection,
    context: PlanContext,
) -> dict[str, Any]:
    row = _normalize_check(template, context)
    row.update(
        {
            "id": selection.row_id,
            "name": selection.leaf_name,
            "command": selection.command,
            "template_id": selection.template_id,
            "upload_xtask_bin_artifact": False,
            "download_xtask_bin_artifact": False,
        }
    )
    return row


def _is_enabled(check: dict[str, Any], context: PlanContext) -> bool:
    required_owner = check.get("required_owner")
    if required_owner and context.repository_owner != required_owner:
        return False

    required_base = check.get("required_base_branch")
    if required_base and (
        context.event_name != "pull_request" or context.base_ref != required_base
    ):
        return False

    events = check.get("events", [])
    boolean_input = check.get("enable_boolean_input")
    if events or boolean_input:
        event_matches = context.event_name in events
        input_matches = boolean_input in context.enabled_boolean_inputs
        if not event_matches and not input_matches:
            return False

    return True


def _matches_impact(check: dict[str, Any], context: PlanContext) -> bool:
    impact = context.impact
    if context.event_name != "pull_request" or impact is None or impact.full:
        return True

    impact_targets = set(check.get("impact_targets", ()))
    impact_packages = set(check.get("impact_packages", ()))
    if not impact_targets and not impact_packages:
        return True

    if any(
        target.partition(":")[0] in impact.affected_oses for target in impact_targets
    ):
        return True
    input_matches = any(
        check_matches_input(check, selection) for selection in impact.input_selections
    )
    if input_matches:
        return True
    if impact.input_selections:
        return bool(impact_packages.intersection(impact.affected_packages))

    return bool(
        impact_targets.intersection(impact.targets)
        or impact_packages.intersection(impact.affected_packages)
    )


def _suite_path_os(path: str) -> str | None:
    normalized = Path(path)
    prefixes = {
        Path("test-suit/arceos"): "arceos",
        Path("test-suit/starryos"): "starry",
        Path("test-suit/axvisor"): "axvisor",
    }
    for prefix, os_name in prefixes.items():
        if normalized == prefix or prefix in normalized.parents:
            return os_name
    return None


def _normalize_check(check: dict[str, Any], context: PlanContext) -> dict[str, Any]:
    runs_on = list(check["runs_on"])
    environment = check["environment"]
    fallback = bool(
        check.get("self_hosted_owner")
        and context.repository_owner != check["self_hosted_owner"]
    )
    if fallback:
        runs_on = ["ubuntu-latest"]
        environment = check["fallback_environment"]

    fetch_depth = str(check.get("fetch_depth", "1"))
    if fetch_depth == "full":
        fetch_depth = "2" if "self-hosted" in runs_on and not fallback else "0"

    container_image = _container_image(environment, context.repository)
    preflight = check.get("container_preflight")
    if preflight is None:
        preflight = "full" if container_image else "none"

    download_xtask = check.get("download_xtask_bin_artifact", False)
    if fallback and check["phase"] == "test":
        download_xtask = True

    command = check["command"]
    if (
        context.event_name == "pull_request"
        and context.impact is not None
        and not context.impact.full
        and check.get("pull_request_command")
    ):
        command = check["pull_request_command"]

    return {
        "id": check["id"],
        "name": check["name"],
        "group": check["group"],
        "runs_on": runs_on,
        "container_image": container_image,
        "container_preflight": preflight,
        "command": command.strip(),
        "cache_key": check.get("cache_key", ""),
        "apk_region": check.get("apk_region", "china"),
        "wifi_secrets": check.get("wifi_secrets", False),
        "fetch_depth": fetch_depth,
        "timeout_minutes": check.get("timeout_minutes", 360),
        "require_kvm": check.get("require_kvm", False),
        "upload_xtask_bin_artifact": check.get("upload_xtask_bin_artifact", False),
        "download_xtask_bin_artifact": download_xtask,
        "xtask_bin_artifact_name": check.get("xtask_bin_artifact_name", "tg-xtask-bin"),
        "source": check["source"],
    }


def _container_image(environment: str, repository: str) -> str:
    repository = repository.lower()
    if environment == "host":
        return ""
    if environment == "base":
        return f"ghcr.io/{repository}-container:latest"
    if environment == "axvisor-lvz":
        return f"ghcr.io/{repository}-container-axvisor-lvz:latest"
    raise PlanError(f"unsupported environment: {environment}")


def _matrix_rows(outputs: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        row
        for value in outputs.values()
        if isinstance(value, dict) and isinstance(value.get("include"), list)
        for row in value["include"]
    ]


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Plan TGOSKits CI matrices")
    parser.add_argument("--mode", choices=("main", "starry-apps"), required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--repository-owner", required=True)
    parser.add_argument("--event-name", required=True)
    parser.add_argument("--base-ref", default="")
    parser.add_argument("--since-ref", default="")
    parser.add_argument("--boolean-input", action="append", default=[])
    parser.add_argument(
        "--output-file",
        type=Path,
        default=(
            Path(os.environ["GITHUB_OUTPUT"]) if "GITHUB_OUTPUT" in os.environ else None
        ),
    )
    parser.add_argument(
        "--summary-file",
        type=Path,
        default=(
            Path(os.environ["GITHUB_STEP_SUMMARY"])
            if "GITHUB_STEP_SUMMARY" in os.environ
            else None
        ),
    )
    return parser.parse_args()


def main() -> int:
    args = _parse_args()
    impact = None
    if args.mode == "main" and args.event_name == "pull_request":
        impact = analyze_pull_request(WORKSPACE_ROOT, args.since_ref)
    context = PlanContext(
        repository=args.repository,
        repository_owner=args.repository_owner,
        event_name=args.event_name,
        base_ref=args.base_ref,
        enabled_boolean_inputs=frozenset(args.boolean_input),
        impact=impact,
    )
    try:
        if args.mode == "main":
            checks = load_catalog(MAIN_MANIFESTS)
            context = _resolve_input_fallbacks(checks, context)
            impact = context.impact
            outputs = _build_main_plan(checks, context)
        else:
            outputs = build_starry_apps_plan(context)
    except (OSError, PlanError, tomllib.TOMLDecodeError) as error:
        print(f"CI planning failed: {error}", file=sys.stderr)
        return 1

    if args.output_file is not None:
        write_github_outputs(outputs, args.output_file)
    else:
        print(json.dumps(outputs, indent=2))

    for name, value in outputs.items():
        if isinstance(value, dict) and isinstance(value.get("include"), list):
            print(f"{name}: {len(value['include'])} checks", file=sys.stderr)
    if impact is not None:
        rows = _matrix_rows(outputs)
        selected_ids = [row["id"] for row in rows]
        selected_template_ids = {
            row.get("template_id", row["id"]) for row in rows
        }
        eligible_ids = [
            check["id"]
            for check in checks
            if _is_enabled(check, context)
        ]
        skipped_ids = [
            check_id
            for check_id in eligible_ids
            if check_id not in selected_template_ids
        ]
        summary = render_summary(impact, selected_ids, skipped_ids)
        print(summary, file=sys.stderr)
        if args.summary_file is not None:
            with args.summary_file.open("a", encoding="utf-8") as summary_file:
                summary_file.write(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main())
