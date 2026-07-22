#!/usr/bin/env python3

import sys
from pathlib import Path


WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
CI_WORKFLOW = WORKSPACE_ROOT / ".github/workflows/ci.yml"
STARRY_COVERAGE_TARGETS = (
    ("x86_64", "x86_64-unknown-linux-musl"),
    ("aarch64", "aarch64-unknown-linux-musl"),
    ("riscv64", "riscv64gc-unknown-linux-musl"),
    ("loongarch64", "loongarch64-unknown-linux-musl"),
)


def main() -> int:
    command = starry_coverage_command()
    failures = coverage_cleanup_failures(command)
    if not failures:
        return 0

    print("Starry coverage CI can exhaust the hosted runner disk:", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    return 1


def starry_coverage_command() -> str:
    workflow = CI_WORKFLOW.read_text(encoding="utf-8")
    job = workflow.split("          - name: Coverage test starry\n", maxsplit=1)[1]
    job = job.split("          - name:", maxsplit=1)[0]
    return job.split("            command: |\n", maxsplit=1)[1].split(
        "            cache_key:", maxsplit=1
    )[0]


def coverage_cleanup_failures(command: str) -> list[str]:
    failures = []
    for index, (arch, target) in enumerate(STARRY_COVERAGE_TARGETS):
        coverage = f"--arch {arch} --coverage --out-fmt html"
        cleanup = f"cargo clean --target-dir target/{target}"
        coverage_index = command.find(coverage)
        cleanup_index = command.find(cleanup)
        next_coverage_index = next_coverage_start(command, index)

        if coverage_index < 0:
            failures.append(f"missing coverage run for {arch}")
        elif cleanup_index < coverage_index or (
            next_coverage_index >= 0 and cleanup_index > next_coverage_index
        ):
            failures.append(
                f"{arch} must clean target {target} before the next coverage run"
            )
    return failures


def next_coverage_start(command: str, index: int) -> int:
    if index + 1 == len(STARRY_COVERAGE_TARGETS):
        return -1
    next_arch = STARRY_COVERAGE_TARGETS[index + 1][0]
    return command.find(f"--arch {next_arch} --coverage --out-fmt html")


if __name__ == "__main__":
    sys.exit(main())
