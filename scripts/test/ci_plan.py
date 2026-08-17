#!/usr/bin/env python3

import argparse
import json
import os
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


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

SUPPORTED_PHASES = {"static", "test", "starry_apps"}
SUPPORTED_ENVIRONMENTS = {"host", "base", "axvisor-lvz"}
SUPPORTED_PREFLIGHTS = {"none", "qemu-user", "full"}
CHECK_ID_PATTERN = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
REDUNDANT_NAME_PREFIX_PATTERN = re.compile(
    r"^(?:check|run|scheduled|tests?)\b", re.IGNORECASE
)
TOP_LEVEL_FIELDS = {"schema_version", "phase", "group", "check"}
CHECK_FIELDS = {
    "id",
    "name",
    "runs_on",
    "environment",
    "command",
    "self_hosted_owner",
    "fallback_environment",
    "required_owner",
    "required_base_branch",
    "fetch_depth",
    "timeout_minutes",
    "require_kvm",
    "cache_key",
    "apk_region",
    "upload_xtask_bin_artifact",
    "download_xtask_bin_artifact",
    "xtask_bin_artifact_name",
    "container_preflight",
    "events",
    "enable_boolean_input",
}
REQUIRED_CHECK_FIELDS = {"id", "name", "runs_on", "environment", "command"}
BOOLEAN_CHECK_FIELDS = {
    "require_kvm",
    "upload_xtask_bin_artifact",
    "download_xtask_bin_artifact",
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


def load_catalog(manifests: Iterable[Path]) -> list[dict[str, Any]]:
    checks: list[dict[str, Any]] = []
    seen_ids: set[str] = set()

    for manifest in manifests:
        document = _load_manifest(manifest)
        phase = document["phase"]
        group = document["group"]
        for index, raw_check in enumerate(document["check"], start=1):
            location = f"{manifest}:{index}"
            check = _validate_check(raw_check, location, group)
            check_id = check["id"]
            if check_id in seen_ids:
                raise PlanError(f"duplicate check id '{check_id}' at {location}")
            seen_ids.add(check_id)
            check["phase"] = phase
            check["group"] = group
            check["source"] = str(manifest.relative_to(WORKSPACE_ROOT))
            checks.append(check)

    _validate_artifact_contract(checks)
    return checks


def build_main_plan(context: PlanContext) -> dict[str, dict[str, list[dict[str, Any]]]]:
    checks = load_catalog(MAIN_MANIFESTS)
    static_rows = _plan_phase(checks, "static", context)
    test_rows = _plan_phase(checks, "test", context)
    if not static_rows or not test_rows:
        raise PlanError("main CI must resolve to non-empty static and test matrices")
    return {
        "static_matrix": {"include": static_rows},
        "test_matrix": {"include": test_rows},
    }


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
    if document.get("schema_version") != 1:
        raise PlanError(f"{manifest} must declare schema_version = 1")
    if document.get("phase") not in SUPPORTED_PHASES:
        raise PlanError(f"{manifest} has an unsupported phase")
    if not isinstance(document.get("group"), str) or not document["group"].strip():
        raise PlanError(f"{manifest} must declare a non-empty group")
    if not isinstance(document.get("check"), list) or not document["check"]:
        raise PlanError(f"{manifest} must contain at least one [[check]] entry")
    return document


def _validate_check(
    raw_check: Any, location: str, group: str = ""
) -> dict[str, Any]:
    if not isinstance(raw_check, dict):
        raise PlanError(f"{location} check entry must be a table")
    unknown_fields = set(raw_check) - CHECK_FIELDS
    if unknown_fields:
        raise PlanError(f"{location} has unknown fields: {sorted(unknown_fields)}")
    missing_fields = REQUIRED_CHECK_FIELDS - set(raw_check)
    if missing_fields:
        raise PlanError(f"{location} is missing fields: {sorted(missing_fields)}")

    check = dict(raw_check)
    for field in ("id", "name", "environment", "command"):
        if not isinstance(check[field], str) or not check[field].strip():
            raise PlanError(f"{location} field '{field}' must be a non-empty string")
    check["name"] = check["name"].strip()
    _validate_display_name(check["name"], group, location)
    if CHECK_ID_PATTERN.fullmatch(check["id"]) is None:
        raise PlanError(f"{location} field 'id' must use lowercase kebab-case")
    if check["environment"] not in SUPPORTED_ENVIRONMENTS:
        raise PlanError(f"{location} has an unsupported environment")

    runs_on = check["runs_on"]
    if (
        not isinstance(runs_on, list)
        or not runs_on
        or any(not isinstance(label, str) or not label for label in runs_on)
    ):
        raise PlanError(f"{location} runs_on must be a non-empty string array")

    fallback_environment = check.get("fallback_environment")
    if fallback_environment is not None:
        if not check.get("self_hosted_owner"):
            raise PlanError(
                f"{location} fallback_environment requires self_hosted_owner"
            )
        if fallback_environment not in SUPPORTED_ENVIRONMENTS - {"host"}:
            raise PlanError(f"{location} has an unsupported fallback_environment")

    for field in ("self_hosted_owner", "required_owner", "required_base_branch"):
        value = check.get(field)
        if value is not None and (not isinstance(value, str) or not value):
            raise PlanError(f"{location} field '{field}' must be a non-empty string")
    if check.get("self_hosted_owner") and "self-hosted" not in runs_on:
        raise PlanError(f"{location} self_hosted_owner requires a self-hosted runner")

    for field in BOOLEAN_CHECK_FIELDS:
        value = check.get(field)
        if value is not None and not isinstance(value, bool):
            raise PlanError(f"{location} field '{field}' must be boolean")

    timeout = check.get("timeout_minutes", 360)
    if not isinstance(timeout, int) or timeout <= 0:
        raise PlanError(f"{location} timeout_minutes must be positive")

    fetch_depth = str(check.get("fetch_depth", "1"))
    if fetch_depth not in {"0", "1", "2", "full"}:
        raise PlanError(f"{location} has an unsupported fetch_depth")

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
        raise PlanError(
            f"{location} enable_boolean_input must be a non-empty string"
        )

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

    return check


def _validate_display_name(name: str, group: str, location: str) -> None:
    if REDUNDANT_NAME_PREFIX_PATTERN.search(name):
        raise PlanError(
            f"{location} name must not start with Test/Run/Check/Scheduled"
        )
    if "self-hosted" in name.casefold():
        raise PlanError(f"{location} name must not expose the self-hosted runner")
    if group:
        group_pattern = re.compile(
            rf"(?<![A-Za-z0-9]){re.escape(group)}(?![A-Za-z0-9])",
            re.IGNORECASE,
        )
        if group_pattern.search(name):
            raise PlanError(f"{location} name must not repeat group '{group}'")


def _validate_artifact_contract(checks: list[dict[str, Any]]) -> None:
    main_checks = [check for check in checks if check["phase"] in {"static", "test"}]
    if not main_checks:
        return

    producers = [
        check for check in main_checks if check.get("upload_xtask_bin_artifact", False)
    ]
    if len(producers) != 1 or producers[0]["phase"] != "static":
        raise PlanError("main CI must define exactly one static xtask artifact producer")

    producer_name = producers[0].get("xtask_bin_artifact_name", "tg-xtask-bin")
    for check in main_checks:
        if not check.get("download_xtask_bin_artifact", False):
            continue
        consumer_name = check.get("xtask_bin_artifact_name", "tg-xtask-bin")
        if consumer_name != producer_name:
            raise PlanError(
                f"check '{check['id']}' consumes unknown artifact '{consumer_name}'"
            )


def _plan_phase(
    checks: list[dict[str, Any]], phase: str, context: PlanContext
) -> list[dict[str, Any]]:
    return [
        _normalize_check(check, context)
        for check in checks
        if check["phase"] == phase and _is_enabled(check, context)
    ]


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


def _normalize_check(
    check: dict[str, Any], context: PlanContext
) -> dict[str, Any]:
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

    name = check["name"]
    if check["phase"] == "test":
        name = f"{check['group']} / {name}"

    download_xtask = check.get("download_xtask_bin_artifact", False)
    if fallback and check["phase"] == "test":
        download_xtask = True

    return {
        "id": check["id"],
        "name": name,
        "group": check["group"],
        "runs_on": runs_on,
        "container_image": container_image,
        "container_preflight": preflight,
        "command": check["command"].strip(),
        "cache_key": check.get("cache_key", ""),
        "apk_region": check.get("apk_region", "china"),
        "fetch_depth": fetch_depth,
        "timeout_minutes": check.get("timeout_minutes", 360),
        "require_kvm": check.get("require_kvm", False),
        "upload_xtask_bin_artifact": check.get(
            "upload_xtask_bin_artifact", False
        ),
        "download_xtask_bin_artifact": download_xtask,
        "xtask_bin_artifact_name": check.get(
            "xtask_bin_artifact_name", "tg-xtask-bin"
        ),
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


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Plan TGOSKits CI matrices")
    parser.add_argument("--mode", choices=("main", "starry-apps"), required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--repository-owner", required=True)
    parser.add_argument("--event-name", required=True)
    parser.add_argument("--base-ref", default="")
    parser.add_argument("--boolean-input", action="append", default=[])
    parser.add_argument(
        "--output-file",
        type=Path,
        default=(
            Path(os.environ["GITHUB_OUTPUT"])
            if "GITHUB_OUTPUT" in os.environ
            else None
        ),
    )
    return parser.parse_args()


def main() -> int:
    args = _parse_args()
    context = PlanContext(
        repository=args.repository,
        repository_owner=args.repository_owner,
        event_name=args.event_name,
        base_ref=args.base_ref,
        enabled_boolean_inputs=frozenset(args.boolean_input),
    )
    try:
        if args.mode == "main":
            outputs = build_main_plan(context)
        else:
            outputs = build_starry_apps_plan(context)
    except (OSError, PlanError, tomllib.TOMLDecodeError) as error:
        print(f"CI planning failed: {error}", file=sys.stderr)
        return 1

    if args.output_file is not None:
        write_github_outputs(outputs, args.output_file)
    else:
        print(json.dumps(outputs, indent=2))

    for name, matrix in outputs.items():
        print(f"{name}: {len(matrix['include'])} checks", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
