#!/usr/bin/env python3

import re
from pathlib import Path
from typing import Any

import tomllib

GLOBAL_DEFAULT_RUNNER = "ubuntu-base"
SUPPORTED_ENVIRONMENTS = {"host", "base", "axvisor-lvz"}
PROFILE_ID_PATTERN = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
TOP_LEVEL_FIELDS = {"schema_version", "profile"}
PROFILE_FIELDS = {
    "runs_on",
    "environment",
    "self_hosted_owner",
    "fallback_environment",
    "required_owner",
    "require_kvm",
}
REQUIRED_PROFILE_FIELDS = {"runs_on", "environment"}


class RunnerProfileError(ValueError):
    """Raised when the shared runner profile manifest is invalid."""


def load_runner_profiles(manifest: Path) -> dict[str, dict[str, Any]]:
    if not manifest.is_file():
        raise RunnerProfileError(f"missing CI runner profile manifest: {manifest}")
    with manifest.open("rb") as source:
        document = tomllib.load(source)

    unknown_fields = set(document) - TOP_LEVEL_FIELDS
    if unknown_fields:
        raise RunnerProfileError(
            f"{manifest} has unknown top-level fields: {sorted(unknown_fields)}"
        )
    if document.get("schema_version") != 3:
        raise RunnerProfileError(f"{manifest} must declare schema_version = 3")
    profiles = document.get("profile")
    if not isinstance(profiles, dict) or not profiles:
        raise RunnerProfileError(f"{manifest} must define at least one runner profile")

    validated = {}
    for name, raw_profile in profiles.items():
        location = f"{manifest} profile '{name}'"
        if PROFILE_ID_PATTERN.fullmatch(name) is None:
            raise RunnerProfileError(f"{location} name must use lowercase kebab-case")
        if not isinstance(raw_profile, dict):
            raise RunnerProfileError(f"{location} must be a table")
        unknown_profile_fields = set(raw_profile) - PROFILE_FIELDS
        if unknown_profile_fields:
            raise RunnerProfileError(
                f"{location} has unknown fields: {sorted(unknown_profile_fields)}"
            )
        missing_profile_fields = REQUIRED_PROFILE_FIELDS - set(raw_profile)
        if missing_profile_fields:
            raise RunnerProfileError(
                f"{location} is missing fields: {sorted(missing_profile_fields)}"
            )
        profile = dict(raw_profile)
        validate_runner_profile(profile, location)
        validated[name] = profile

    if GLOBAL_DEFAULT_RUNNER not in validated:
        raise RunnerProfileError(
            f"{manifest} must define the global runner '{GLOBAL_DEFAULT_RUNNER}'"
        )
    return validated


def validate_runner_profile(profile: dict[str, Any], location: str) -> None:
    runs_on = profile["runs_on"]
    if (
        not isinstance(runs_on, list)
        or not runs_on
        or any(not isinstance(label, str) or not label for label in runs_on)
    ):
        raise RunnerProfileError(f"{location} runs_on must be a non-empty string array")
    environment = profile["environment"]
    if not isinstance(environment, str) or environment not in SUPPORTED_ENVIRONMENTS:
        raise RunnerProfileError(f"{location} has an unsupported environment")
    for field in ("self_hosted_owner", "required_owner"):
        value = profile.get(field)
        if value is not None and (not isinstance(value, str) or not value):
            raise RunnerProfileError(
                f"{location} field '{field}' must be a non-empty string"
            )
    if profile.get("self_hosted_owner") and "self-hosted" not in runs_on:
        raise RunnerProfileError(
            f"{location} self_hosted_owner requires a self-hosted runner"
        )
    fallback = profile.get("fallback_environment")
    if fallback is not None:
        if not profile.get("self_hosted_owner"):
            raise RunnerProfileError(
                f"{location} fallback_environment requires self_hosted_owner"
            )
        if not isinstance(fallback, str) or fallback not in SUPPORTED_ENVIRONMENTS - {
            "host"
        }:
            raise RunnerProfileError(
                f"{location} has an unsupported fallback_environment"
            )
    require_kvm = profile.get("require_kvm")
    if require_kvm is not None and not isinstance(require_kvm, bool):
        raise RunnerProfileError(f"{location} field 'require_kvm' must be boolean")
