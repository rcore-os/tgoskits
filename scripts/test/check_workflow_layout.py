#!/usr/bin/env python3

import re
import sys
from pathlib import Path


WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = WORKSPACE_ROOT / ".github" / "workflows"
CHECKS = WORKSPACE_ROOT / ".github" / "ci" / "checks"
PUBLISH_ACTION = (
    WORKSPACE_ROOT / ".github" / "actions" / "publish-container" / "action.yml"
)


def main() -> int:
    errors: list[str] = []
    affected_workflows = (
        "ci-branch-push.yml",
        "ci.yml",
        "container-publish.yml",
        "release-plz.yml",
        "reusable-check-matrix.yml",
        "starry-apps.yml",
    )
    texts: dict[str, str] = {}
    for name in affected_workflows:
        path = WORKFLOWS / name
        if not path.is_file():
            errors.append(f"missing workflow: .github/workflows/{name}")
            continue
        text = path.read_text(encoding="utf-8")
        texts[name] = text
        if re.search(r"^    if:", text, flags=re.MULTILINE):
            errors.append(f"{name} contains a job-level applicability condition")

    reusable = texts.get("reusable-check-matrix.yml", "")
    if count_jobs(reusable) != 1:
        errors.append("reusable-check-matrix.yml must define exactly one job")
    for legacy_name in ("run_host:", "run_container:", "reusable-command.yml"):
        if legacy_name in reusable:
            errors.append(f"reusable matrix retains legacy branch: {legacy_name}")

    ci = texts.get("ci.yml", "")
    for required_call in (
        "name: Static",
        "name: Tests",
        "uses: ./.github/workflows/reusable-check-matrix.yml",
    ):
        if required_call not in ci:
            errors.append(f"main CI is missing: {required_call}")
    if "dorny/paths-filter" in ci:
        errors.append("main CI must not create a detect-and-skip path-filter job")

    starry_apps = texts.get("starry-apps.yml", "")
    if "scheduled_clippy_all:" in starry_apps:
        errors.append("Starry Apps must select clippy through its planned matrix")
    if "  checks:\n    name: Starry Apps\n" not in starry_apps:
        errors.append("Starry Apps reusable call must use the concise display name")
    if re.search(r"^  pull_request:", starry_apps, flags=re.MULTILINE):
        errors.append("Starry Apps must not run on pull requests")
    for required_event in ("schedule", "workflow_dispatch"):
        if not re.search(rf"^  {required_event}:", starry_apps, flags=re.MULTILINE):
            errors.append(f"Starry Apps must run on {required_event}")

    container = texts.get("container-publish.yml", "")
    if count_jobs(container) != 1:
        errors.append("container-publish.yml must expose one publish job")
    if "uses: ./.github/actions/publish-container" not in container:
        errors.append("container publishing must use the shared composite action")
    if not PUBLISH_ACTION.is_file():
        errors.append("missing publish-container composite action")
    else:
        action = PUBLISH_ACTION.read_text(encoding="utf-8")
        for required_fragment in (
            "using: composite",
            "uses: docker/metadata-action@v6",
            "uses: docker/build-push-action@v7",
            "cache-from: type=gha,scope=${{ inputs.cache_scope }}",
            "cache-to: type=gha,mode=max,scope=${{ inputs.cache_scope }}",
        ):
            if required_fragment not in action:
                errors.append(
                    f"publish-container action is missing: {required_fragment}"
                )

    for manifest in CHECKS.glob("*.toml"):
        if "${{" in manifest.read_text(encoding="utf-8"):
            errors.append(f"{manifest.name} embeds a GitHub expression")

    legacy_workflow = WORKFLOWS / "reusable-command.yml"
    if legacy_workflow.exists():
        errors.append("legacy reusable-command.yml must be removed")

    return report(errors)


def count_jobs(workflow: str) -> int:
    in_jobs = False
    count = 0
    for line in workflow.splitlines():
        if line == "jobs:":
            in_jobs = True
            continue
        if in_jobs and line and not line.startswith(" "):
            break
        if in_jobs and re.fullmatch(r"  [A-Za-z_][A-Za-z0-9_-]*:", line):
            count += 1
    return count


def report(errors: list[str]) -> int:
    if not errors:
        print("Workflow layout checks passed")
        return 0
    print("Workflow layout checks failed:", file=sys.stderr)
    for error in errors:
        print(f"  - {error}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
