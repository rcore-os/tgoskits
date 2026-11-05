#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
fail() {
    echo "selfbuild contract: $*" >&2
    exit 1
}

# Exercise the real guard: a fresh build is accepted, while existing logs or
# targets are rejected without modifying their contents.
mkdir -p "$repo_root/tmp"
cold_run_test_dir="$(mktemp -d -p "$repo_root/tmp" cold-run-contract.XXXXXX)"
trap 'rm -r -- "$cold_run_test_dir"' EXIT
cold_run_guard="$app_dir/check_cold_run.sh"
bash "$cold_run_guard" "$cold_run_test_dir/new-run" \
    "$cold_run_test_dir/new-target" SELFBUILD-CONTRACT \
    || fail "cold-run guard rejected unused directories"
mkdir "$cold_run_test_dir/used-run" "$cold_run_test_dir/used-target"
printf preserved > "$cold_run_test_dir/used-run/run.log"
printf preserved > "$cold_run_test_dir/used-target/artifact"
if bash "$cold_run_guard" "$cold_run_test_dir/used-run" \
    "$cold_run_test_dir/new-target" SELFBUILD-CONTRACT \
    > "$cold_run_test_dir/run-output"; then
    fail "cold-run guard accepted a duplicate run id"
fi
grep -Fxq '===SELFBUILD-CONTRACT-FAIL reason=run-directory-already-exists===' \
    "$cold_run_test_dir/run-output" \
    || fail "duplicate run id did not report the expected reason"
if bash "$cold_run_guard" "$cold_run_test_dir/another-new-run" \
    "$cold_run_test_dir/used-target" SELFBUILD-CONTRACT \
    > "$cold_run_test_dir/target-output"; then
    fail "cold-run guard accepted an existing target"
fi
grep -Fxq '===SELFBUILD-CONTRACT-FAIL reason=target-directory-already-exists===' \
    "$cold_run_test_dir/target-output" \
    || fail "existing target did not report the expected reason"
[ "$(cat "$cold_run_test_dir/used-run/run.log")" = preserved ] || fail "existing log changed"
[ "$(cat "$cold_run_test_dir/used-target/artifact")" = preserved ] || fail "existing artifact changed"
echo "orangepi5plus_selfbuild_contract=PASS"
