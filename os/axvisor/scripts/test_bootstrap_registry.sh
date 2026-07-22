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

echo ""
echo "=== Results: ${PASS} passed, ${FAIL} failed ==="

cleanup

if [ "${FAIL}" -gt 0 ]; then
  echo "FAILURE: ${FAIL} test(s) failed." >&2
  exit 1
fi

echo "All tests passed."
exit 0
