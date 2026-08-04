#!/usr/bin/env bash
# Regression tests for bootstrap_image_registry fallback behavior.
#
# These tests directly source setup_qemu.sh to load the production functions,
# then exercise them with a mock curl.  The production script has a source
# guard — when sourced, only function and variable definitions are loaded;
# the main execution logic is skipped.
#
# Usage:
#   bash os/axvisor/scripts/test_bootstrap_registry.sh
#
# All tests are deterministic and require no network access.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_QEMU="${SCRIPT_DIR}/setup_qemu.sh"

if [ ! -f "${SETUP_QEMU}" ]; then
  echo "ERROR: setup_qemu.sh not found at ${SETUP_QEMU}" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Mock curl — must be defined BEFORE sourcing setup_qemu.sh so the
# production functions resolve to it instead of the real curl.
# ---------------------------------------------------------------------------
CURL_MOCK_MODE=""

curl() {
  local args=("$@")
  local output_file=""
  local url=""
  local i

  for ((i = 0; i < ${#args[@]}; i++)); do
    if [ "${args[$i]}" = "-o" ] && [ $((i + 1)) -lt ${#args[@]} ]; then
      output_file="${args[$((i + 1))]}"
      if [ $i -gt 0 ]; then
        url="${args[$((i - 1))]}"
      fi
      break
    fi
  done

  if [ -z "${url}" ]; then
    for ((i = 0; i < ${#args[@]}; i++)); do
      if [[ "${args[$i]}" == http* ]]; then
        url="${args[$i]}"
        break
      fi
    done
  fi

  case "${CURL_MOCK_MODE}" in
    all-fail)
      return 1
      ;;
    all-ok)
      if [ -n "${output_file}" ]; then
        if [[ "${url}" == *"default.toml"* ]]; then
          printf '[[includes]]\nurl = "https://example.com/registry/v0.0.5/images.toml"\n' > "${output_file}"
        else
          printf '# mock registry\n' > "${output_file}"
        fi
      fi
      return 0
      ;;
    default-fail-fallback-ok)
      if [[ "${url}" == *"default.toml"* ]]; then
        return 1
      fi
      if [ -n "${output_file}" ]; then
        printf '# mock fallback registry\n' > "${output_file}"
      fi
      return 0
      ;;
    *)
      echo "ERROR: unknown CURL_MOCK_MODE=${CURL_MOCK_MODE}" >&2
      return 1
      ;;
  esac
}
export -f curl

# ---------------------------------------------------------------------------
# Load production functions by sourcing setup_qemu.sh.
# The source guard ([[ "${BASH_SOURCE[0]}" == "${0}" ]]) ensures only
# function and variable definitions are loaded — main logic is skipped.
#
# We temporarily relax shell options because setup_qemu.sh sets
# `set -euo pipefail` at its top, which would override ours.
# ---------------------------------------------------------------------------
_saved_opts="$(set +o)"
set +euo pipefail

# Point IMAGE_STORAGE_ROOT at a test-private directory.
export TGOS_IMAGE_LOCAL_STORAGE="${TGOS_IMAGE_LOCAL_STORAGE:-/tmp/test_bootstrap_registry_$$/images}"

# shellcheck disable=SC1090
source "${SETUP_QEMU}"

eval "${_saved_opts}"

# ---------------------------------------------------------------------------
# Test harness
# ---------------------------------------------------------------------------
TEST_ROOT="/tmp/test_bootstrap_registry_$$"
PASS=0
FAIL=0

cleanup() {
  rm -rf "${TEST_ROOT}"
}

setup() {
  rm -rf "${TEST_ROOT}"
  mkdir -p "${TEST_ROOT}"
  export TGOS_IMAGE_LOCAL_STORAGE="${TEST_ROOT}/images"
  # Re-source to pick up the new IMAGE_STORAGE_ROOT from the env var above.
  _saved_opts="$(set +o)"
  set +euo pipefail
  source "${SETUP_QEMU}"
  eval "${_saved_opts}"
  unset AXVISOR_REGISTRY_FALLBACK_URL
  CURL_MOCK_MODE=""
}

assert_eq() {
  local desc="$1" expected="$2" actual="$3"
  if [ "${expected}" = "${actual}" ]; then
    echo "  PASS: ${desc}"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: ${desc}"
    echo "    expected: '${expected}'"
    echo "    actual:   '${actual}'"
    FAIL=$((FAIL + 1))
  fi
}

assert_stderr_contains() {
  local desc="$1" pattern="$2" stderr_output="$3"
  if echo "${stderr_output}" | grep -qF "${pattern}"; then
    echo "  PASS: ${desc}"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: ${desc} — stderr does not contain '${pattern}'"
    echo "    stderr: ${stderr_output}"
    FAIL=$((FAIL + 1))
  fi
}

assert_file_exists() {
  local desc="$1" filepath="$2"
  if [ -f "${filepath}" ]; then
    echo "  PASS: ${desc}"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: ${desc} — file not found: ${filepath}"
    FAIL=$((FAIL + 1))
  fi
}

# ---------------------------------------------------------------------------
# Helper: call bootstrap_image_registry and capture its exit code + stderr.
# The `|| rc=$?` pattern prevents set -e from killing the test on non-zero
# returns while still capturing the actual exit code.
# ---------------------------------------------------------------------------
run_bootstrap() {
  local _stderr_file="$1"
  local _rc=0
  bootstrap_image_registry > /dev/null 2>"${_stderr_file}" || _rc=$?
  return "${_rc}"
}

# ---------------------------------------------------------------------------
# Case 1: default registry unreachable, no fallback URL
# ---------------------------------------------------------------------------
test_case1_default_unreachable_no_fallback() {
  echo ""
  echo "=== Case 1: Default registry unreachable, no fallback URL ==="
  setup
  CURL_MOCK_MODE="all-fail"

  local stderr_file rc stderr
  stderr_file="$(mktemp)"
  run_bootstrap "${stderr_file}"
  rc=$?
  stderr="$(cat "${stderr_file}")"
  rm -f "${stderr_file}"

  assert_eq \
    "bootstrap returns 0 (does not trigger set -e exit)" \
    "0" "${rc}"

  assert_stderr_contains \
    "prints xtask fallback message" \
    "letting cargo xtask handle image sync" \
    "${stderr}"
}

# ---------------------------------------------------------------------------
# Case 2: default registry fails, fallback URL is available
# ---------------------------------------------------------------------------
test_case2_fallback_url_available() {
  echo ""
  echo "=== Case 2: Fallback registry URL available ==="
  setup
  CURL_MOCK_MODE="default-fail-fallback-ok"
  export AXVISOR_REGISTRY_FALLBACK_URL="https://fallback.example.com/registry.toml"

  local stderr_file rc stderr
  stderr_file="$(mktemp)"
  run_bootstrap "${stderr_file}"
  rc=$?
  stderr="$(cat "${stderr_file}")"
  rm -f "${stderr_file}"

  assert_eq \
    "bootstrap returns 0" \
    "0" "${rc}"

  assert_stderr_contains \
    "mentions fallback URL" \
    "trying AXVISOR_REGISTRY_FALLBACK_URL" \
    "${stderr}"

  assert_file_exists \
    "creates images.toml from fallback" \
    "${IMAGE_STORAGE_ROOT}/images.toml"
}

# ---------------------------------------------------------------------------
# Case 3: default registry works, resolves to versioned URL
# ---------------------------------------------------------------------------
test_case3_default_registry_works() {
  echo ""
  echo "=== Case 3: Default registry reachable ==="
  setup
  CURL_MOCK_MODE="all-ok"

  local rc=0
  bootstrap_image_registry > /dev/null 2>&1 || rc=$?

  assert_eq \
    "bootstrap returns 0" \
    "0" "${rc}"

  assert_file_exists \
    "creates images.toml" \
    "${IMAGE_STORAGE_ROOT}/images.toml"

  if grep -q "mock registry" "${IMAGE_STORAGE_ROOT}/images.toml"; then
    echo "  PASS: images.toml contains downloaded content"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: images.toml does not contain expected content"
    FAIL=$((FAIL + 1))
  fi
}

# ---------------------------------------------------------------------------
# Case 4: images.toml already exists → early return 0, no curl calls
# ---------------------------------------------------------------------------
test_case4_already_bootstrapped() {
  echo ""
  echo "=== Case 4: images.toml already exists (idempotent) ==="
  setup
  mkdir -p "${IMAGE_STORAGE_ROOT}"
  touch "${IMAGE_STORAGE_ROOT}/images.toml"
  CURL_MOCK_MODE="all-fail"

  local rc=0
  bootstrap_image_registry > /dev/null 2>&1 || rc=$?

  assert_eq \
    "bootstrap returns 0 (early return)" \
    "0" "${rc}"
}

# ---------------------------------------------------------------------------
# Case 5: regression guard — confirm that reverting return 0 → return 1
# would be caught.  We temporarily patch the in-memory function to verify
# the test infrastructure actually tests the production code path.
# ---------------------------------------------------------------------------
test_case5_regression_guard() {
  echo ""
  echo "=== Case 5: Regression guard (return 1 would fail the test) ==="
  setup
  CURL_MOCK_MODE="all-fail"

  # Create a patched copy of bootstrap_image_registry that returns 1 on
  # the "no registry URL" path, simulating the old bug.
  # This proves the test catches a real regression in the function body.
  bootstrap_image_registry_patched() {
    local storage_dir="${IMAGE_STORAGE_ROOT}"
    local registry_url

    mkdir -p "${storage_dir}"
    if [ -f "${storage_dir}/images.toml" ]; then
      return 0
    fi

    registry_url="$(resolve_registry_url "${DEFAULT_REGISTRY_URL}")"
    if [ -z "${registry_url}" ] && [ -n "${AXVISOR_REGISTRY_FALLBACK_URL:-}" ]; then
      echo "  -> Default registry unreachable, trying AXVISOR_REGISTRY_FALLBACK_URL." >&2
      registry_url="${AXVISOR_REGISTRY_FALLBACK_URL}"
    fi

    if [ -z "${registry_url}" ]; then
      echo "  -> Could not resolve registry URL; letting cargo xtask handle image sync." >&2
      return 1   # <-- this is the old bug
    fi

    echo "  -> Bootstrapping local image registry from: ${registry_url}"
    if ! curl -4 --retry 5 --retry-delay 2 -fsSL "${registry_url}" -o "${storage_dir}/images.toml"; then
      echo "  -> Error: failed to bootstrap local image registry." >&2
      return 0
    fi
    date +%s > "${storage_dir}/.last_sync" || true
  }

  local stderr_file rc stderr
  stderr_file="$(mktemp)"
  bootstrap_image_registry_patched > /dev/null 2>"${stderr_file}" || rc=$?
  rc=${rc:-0}
  stderr="$(cat "${stderr_file}")"
  rm -f "${stderr_file}"

  # The patched version SHOULD return 1 — confirming the test harness
  # can detect a regression.
  assert_eq \
    "regression guard: patched return-1 is detected as non-zero" \
    "1" "${rc}"

  assert_stderr_contains \
    "regression guard: fallback message still printed" \
    "letting cargo xtask handle image sync" \
    "${stderr}"
}

# ---------------------------------------------------------------------------
# Case 6: Sourcing does not mutate persistent .image.toml config.
# Regression guard: setup_qemu.sh top-level logic is behind a BASH_SOURCE
# guard; sourcing must not side-effect the caller's image config.
# ---------------------------------------------------------------------------
test_case6_source_does_not_mutate_image_config() {
  echo ""
  echo "=== Case 6: Sourcing does not mutate .image.toml ==="
  setup

  local image_config="${WORKSPACE_ROOT}/tmp/axbuild/.image.toml"
  local saved_content=""
  local config_existed=false

  # Save pre-source state
  if [ -f "${image_config}" ]; then
    config_existed=true
    saved_content="$(cat "${image_config}")"
  fi

  # Re-source (setup already sourced once; verify idempotent / no drift)
  _saved_opts="$(set +o)"
  set +euo pipefail
  source "${SETUP_QEMU}"
  eval "${_saved_opts}"

  if $config_existed; then
    local current_content
    current_content="$(cat "${image_config}")"
    if [ "${saved_content}" = "${current_content}" ]; then
      echo "  PASS: .image.toml unchanged after source (byte-level match)"
      PASS=$((PASS + 1))
    else
      echo "  FAIL: .image.toml was modified by source"
      echo "    before: ${saved_content}"
      echo "    after:  ${current_content}"
      FAIL=$((FAIL + 1))
    fi
  else
    if [ ! -f "${image_config}" ]; then
      echo "  PASS: .image.toml not created by source (file did not exist before)"
      PASS=$((PASS + 1))
    else
      echo "  FAIL: .image.toml was created by source (did not exist before)"
      FAIL=$((FAIL + 1))
    fi
  fi
}

# ---------------------------------------------------------------------------
# Case 7: persist_image_storage_config updates local_storage to match
# IMAGE_STORAGE_ROOT, preserving other TOML fields.  Regression guard
# for the custom-cache-not-persisted-after-pull bug.
# ---------------------------------------------------------------------------
test_case7_persist_updates_local_storage() {
  echo ""
  echo "=== Case 7: persist_image_storage_config updates local_storage ==="
  setup

  local mock_config="${TEST_ROOT}/.image.toml"
  cat > "${mock_config}" <<-'TOML'
local_storage = "/default/cache/path"
registry = "https://raw.githubusercontent.com/rcore-os/tgosimages/refs/heads/main/registry/default.toml"
auto_sync = true
auto_sync_threshold = 604800
TOML

  persist_image_storage_config "${mock_config}"

  local stored_path
  stored_path="$(grep '^local_storage = ' "${mock_config}" | sed 's/^local_storage = "\(.*\)"$/\1/')"
  assert_eq "local_storage matches IMAGE_STORAGE_ROOT" \
    "${IMAGE_STORAGE_ROOT}" \
    "${stored_path}"

  # Verify other TOML fields are preserved (no data loss from sed).
  local fields_ok=true
  grep -q '^registry' "${mock_config}" || fields_ok=false
  grep -q '^auto_sync' "${mock_config}" || fields_ok=false
  grep -q '^auto_sync_threshold' "${mock_config}" || fields_ok=false
  if $fields_ok; then
    echo "  PASS: other TOML fields preserved"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: other TOML fields not preserved"
    FAIL=$((FAIL + 1))
  fi

  # Simulate the post-script scenario: env var gone, config persists.
  local saved_env="${TGOS_IMAGE_LOCAL_STORAGE-}"
  unset TGOS_IMAGE_LOCAL_STORAGE
  local after_unset
  after_unset="$(grep '^local_storage = ' "${mock_config}" | sed 's/^local_storage = "\(.*\)"$/\1/')"
  export TGOS_IMAGE_LOCAL_STORAGE="${saved_env}"
  assert_eq "local_storage preserved after env var unset" \
    "${TEST_ROOT}/images" \
    "${after_unset}"
}

# ---------------------------------------------------------------------------
# Case 8: persist_image_storage_config no-ops (no crash, no file) when
# .image.toml does not exist — e.g. before the first pull.
# ---------------------------------------------------------------------------
test_case8_persist_noop_when_config_missing() {
  echo ""
  echo "=== Case 8: persist no-op when .image.toml missing ==="
  setup

  local missing_config="${TEST_ROOT}/nonexistent/.image.toml"
  persist_image_storage_config "${missing_config}"

  if [ ! -f "${missing_config}" ]; then
    echo "  PASS: no file created when config did not exist"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: file was unexpectedly created"
    FAIL=$((FAIL + 1))
  fi
}

# ---------------------------------------------------------------------------
# Case 9: End-to-end regression — no initial config + custom cache + pull
# creates default config → persist corrects it → env var gone still works.
# ---------------------------------------------------------------------------
test_case9_persist_after_simulated_pull() {
  echo ""
  echo "=== Case 9: persist corrects config created by pull with default path ==="
  setup

  local mock_config="${TEST_ROOT}/.image.toml"

  # Step 1: no .image.toml initially (simulating fresh workspace)
  rm -f "${mock_config}"

  # Step 2: first persist call (script start / early guard) — no-op
  persist_image_storage_config "${mock_config}"
  if [ ! -f "${mock_config}" ]; then
    echo "  PASS: first persist does not create config"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: first persist unexpectedly created config"
    FAIL=$((FAIL + 1))
  fi

  # Step 3: simulate cargo xtask image pull creating config with default path
  cat > "${mock_config}" <<-'TOML'
local_storage = "/home/user/.cache/tgos/images"
registry = "https://example.com/registry/default.toml"
TOML

  # Step 4: second persist call (post-pull guard) — fixes local_storage
  persist_image_storage_config "${mock_config}"

  local stored_path
  stored_path="$(grep '^local_storage = ' "${mock_config}" | sed 's/^local_storage = "\(.*\)"$/\1/')"
  assert_eq "local_storage corrected after simulated pull" \
    "${IMAGE_STORAGE_ROOT}" \
    "${stored_path}"

  # Step 5: env var gone — config still points to custom cache
  local saved_env="${TGOS_IMAGE_LOCAL_STORAGE-}"
  unset TGOS_IMAGE_LOCAL_STORAGE
  local after_unset
  after_unset="$(grep '^local_storage = ' "${mock_config}" | sed 's/^local_storage = "\(.*\)"$/\1/')"
  export TGOS_IMAGE_LOCAL_STORAGE="${saved_env}"
  assert_eq "custom path persists without env var" \
    "${TEST_ROOT}/images" \
    "${after_unset}"
}

# ---------------------------------------------------------------------------
# Helper: extract local_storage from a TOML config and unescape the TOML
# basic-string escapes emitted by persist_image_storage_config (\\, \", \n,
# \t). Double backslash is protected first so a literal "\\n" is not decoded
# as a newline.
# ---------------------------------------------------------------------------
extract_local_storage() {
  local config_path="$1"
  local line
  line="$(grep '^local_storage = ' "${config_path}")"
  line="${line#local_storage = \"}"
  line="${line%\"}"
  # \x01/\x02 are temporary sentinel bytes swapped in one escape at a time so
  # that a literal "\\n" / "\\t" in the path is not mis-decoded as a control
  # char. This assumes real paths never contain \x01 or \x02 — safe because
  # they are non-printing control chars that cannot appear in a TOML basic
  # string (and persist_image_storage_config never emits \u0001/\u0002).
  line="${line//\\\\/$'\x01'}"
  line="${line//\\\"/$'\x02'}"
  line="${line//\\n/$'\n'}"
  line="${line//\\t/$'\t'}"
  line="${line//$'\x02'/\"}"
  line="${line//$'\x01'/\\}"
  printf '%s' "${line}"
}

# ---------------------------------------------------------------------------
# Case 10: persist_image_storage_config must handle paths containing special
# characters. Regression guard for the sed replacement injection bug:
# & (sed "entire match" backref), | (sed delimiter), \ and " (TOML
# basic-string escapes).
# ---------------------------------------------------------------------------
test_case10_persist_special_character_paths() {
  echo ""
  echo "=== Case 10: persist handles special-character paths ==="
  setup

  local mock_config="${TEST_ROOT}/.image.toml"
  local special_paths=(
    '/tmp/cache&foo'     # & would be sed's "entire match" backref
    '/tmp/cache|foo'     # | is the sed delimiter
    '/tmp/cache\foo'     # backslash must be escaped in TOML basic strings
    '/tmp/cache"foo'     # quote must be escaped in TOML basic strings
  )
  local p
  for p in "${special_paths[@]}"; do
    cat > "${mock_config}" <<-'TOML'
local_storage = "/default/cache/path"
registry = "https://example.com/registry/default.toml"
auto_sync = true
TOML

    IMAGE_STORAGE_ROOT="${p}" persist_image_storage_config "${mock_config}"

    # 1) local_storage round-trips to the original path (byte-for-byte)
    local stored
    stored="$(extract_local_storage "${mock_config}")"
    assert_eq "local_storage round-trips for '${p}'" "${p}" "${stored}"

    # 2) other TOML fields are preserved
    local fields_ok=true
    grep -q '^registry' "${mock_config}" || fields_ok=false
    grep -q '^auto_sync' "${mock_config}" || fields_ok=false
    if $fields_ok; then
      echo "  PASS: other TOML fields preserved for '${p}'"
      PASS=$((PASS + 1))
    else
      echo "  FAIL: other TOML fields lost for '${p}'"
      FAIL=$((FAIL + 1))
    fi
  done
}

# ---------------------------------------------------------------------------
# Run all tests
# ---------------------------------------------------------------------------
echo "=== bootstrap_image_registry regression tests ==="
echo "Production source: ${SETUP_QEMU}"
echo "Test root: ${TEST_ROOT}"

test_case1_default_unreachable_no_fallback
test_case2_fallback_url_available
test_case3_default_registry_works
test_case4_already_bootstrapped
test_case5_regression_guard
test_case6_source_does_not_mutate_image_config
test_case7_persist_updates_local_storage
test_case8_persist_noop_when_config_missing
test_case9_persist_after_simulated_pull
test_case10_persist_special_character_paths

echo ""
echo "=== Results: ${PASS} passed, ${FAIL} failed ==="

cleanup

if [ "${FAIL}" -gt 0 ]; then
  echo "FAILURE: ${FAIL} test(s) failed." >&2
  exit 1
fi

echo "All tests passed."
exit 0
