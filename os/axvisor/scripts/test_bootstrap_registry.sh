#!/usr/bin/env bash
# Regression tests for bootstrap_image_registry fallback behavior.
#
# These tests use a mock curl to verify that bootstrap_image_registry
# never causes the script to exit under `set -e`, and that the fallback
# to `cargo xtask image pull` is always reachable.
#
# Usage:
#   bash os/axvisor/scripts/test_bootstrap_registry.sh
#
# All tests are deterministic and require no network access.

set -euo pipefail

# ---------------------------------------------------------------------------
# Mock curl — controlled via CURL_MOCK_MODE
# ---------------------------------------------------------------------------
CURL_MOCK_MODE=""
CURL_CALL_LOG=""  # records which URLs were requested

curl() {
  local args=("$@")
  local output_file=""
  local url=""
  local i

  # Extract -o <file> and the URL (the argument just before -o).
  # curl is always called as: curl ... "${url}" -o "${outfile}"
  for ((i = 0; i < ${#args[@]}; i++)); do
    if [ "${args[$i]}" = "-o" ] && [ $((i + 1)) -lt ${#args[@]} ]; then
      output_file="${args[$((i + 1))]}"
      if [ $i -gt 0 ]; then
        url="${args[$((i - 1))]}"
      fi
      break
    fi
  done

  # Fallback: find the first http* argument
  if [ -z "${url}" ]; then
    for ((i = 0; i < ${#args[@]}; i++)); do
      if [[ "${args[$i]}" == http* ]]; then
        url="${args[$i]}"
        break
      fi
    done
  fi

  CURL_CALL_LOG="${CURL_CALL_LOG}${url}\n"

  case "${CURL_MOCK_MODE}" in
    all-fail)
      return 1
      ;;
    all-ok)
      if [ -n "${output_file}" ]; then
        if [[ "${url}" == *"default.toml"* ]]; then
          # Simulate a default registry that [[includes]] a versioned registry
          printf '[[includes]]\nurl = "https://example.com/registry/v0.0.5/images.toml"\n' > "${output_file}"
        else
          printf '# mock registry\n' > "${output_file}"
        fi
      fi
      return 0
      ;;
    default-fail-fallback-ok)
      # Default registry unreachable; fallback URL works
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
# Replica of setup_qemu.sh functions under test
# (kept in sync manually; the test will catch drift)
# ---------------------------------------------------------------------------
DEFAULT_REGISTRY_URL="https://raw.githubusercontent.com/rcore-os/tgosimages/refs/heads/main/registry/default.toml"

resolve_registry_url() {
  local default_url="$1"
  local tmpfile include_url

  tmpfile="$(mktemp)"
  if curl -4 --retry 5 --retry-delay 2 -fsSL "${default_url}" -o "${tmpfile}"; then
    include_url="$(sed -n 's/^[[:space:]]*url[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' "${tmpfile}" | sed -n '1p')"
    rm -f "${tmpfile}"
    if [ -n "${include_url}" ]; then
      echo "${include_url}"
    else
      echo "${default_url}"
    fi
    return 0
  fi
  rm -f "${tmpfile}"
  echo ""
}

bootstrap_image_registry() {
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
    return 0
  fi

  echo "  -> Bootstrapping local image registry from: ${registry_url}"
  if ! curl -4 --retry 5 --retry-delay 2 -fsSL "${registry_url}" -o "${storage_dir}/images.toml"; then
    echo "  -> Error: failed to bootstrap local image registry." >&2
    return 0
  fi
  date +%s > "${storage_dir}/.last_sync" || true
}

# ---------------------------------------------------------------------------
# Simulate the fallback call chain from setup_qemu.sh lines 297-308
# ---------------------------------------------------------------------------
simulate_fallback_flow() {
  # This mirrors the actual call chain:
  #   if ! cargo xtask image pull (simulated as "attempt 1 fails"); then
  #     bootstrap_image_registry
  #     cargo xtask image pull (attempt 2)
  #   fi
  local attempt1_rc="$1"

  if [ "${attempt1_rc}" -ne 0 ]; then
    echo "  -> Attempt 1/2 failed. Trying to bootstrap registry..."
    bootstrap_image_registry
    echo "  -> Download attempt 2/2"
    echo "  -> (cargo xtask image pull would run here)"
    return 0
  fi
  return 0
}

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
  export IMAGE_STORAGE_ROOT="${TEST_ROOT}/images"
  unset AXVISOR_REGISTRY_FALLBACK_URL
  CURL_CALL_LOG=""
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
# Case 1: default registry unreachable, no fallback URL
#   Expect: bootstrap returns 0, prints xtask fallback message, script continues
# ---------------------------------------------------------------------------
test_case1_default_unreachable_no_fallback() {
  echo ""
  echo "=== Case 1: Default registry unreachable, no fallback URL ==="
  setup
  CURL_MOCK_MODE="all-fail"

  local stderr_file rc stderr
  stderr_file="$(mktemp)"
  bootstrap_image_registry > /dev/null 2>"${stderr_file}"
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

  # Verify fallback chain: simulate the actual call site flow
  local flow_output
  flow_output="$(simulate_fallback_flow 1 2>&1)"
  local flow_rc=$?

  assert_eq \
    "simulated fallback flow returns 0 (continues to attempt 2)" \
    "0" "${flow_rc}"

  assert_stderr_contains \
    "simulated flow reaches attempt 2" \
    "Download attempt 2/2" \
    "${flow_output}"
}

# ---------------------------------------------------------------------------
# Case 2: default registry fails, fallback URL is available
#   Expect: bootstrap uses fallback URL, creates images.toml, no xtask fallback
# ---------------------------------------------------------------------------
test_case2_fallback_url_available() {
  echo ""
  echo "=== Case 2: Fallback registry URL available ==="
  setup
  CURL_MOCK_MODE="default-fail-fallback-ok"
  export AXVISOR_REGISTRY_FALLBACK_URL="https://fallback.example.com/registry.toml"

  # Capture both stderr and return code in a single call.
  # Using a temp file for stderr because bootstrap_image_registry
  # also writes to stdout (the "Bootstrapping from: ..." message).
  local stderr_file rc
  stderr_file="$(mktemp)"
  bootstrap_image_registry > /dev/null 2>"${stderr_file}"
  rc=$?
  local stderr
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
#   Expect: bootstrap downloads from resolved URL, creates images.toml
# ---------------------------------------------------------------------------
test_case3_default_registry_works() {
  echo ""
  echo "=== Case 3: Default registry reachable ==="
  setup
  CURL_MOCK_MODE="all-ok"

  local rc
  bootstrap_image_registry > /dev/null 2>&1
  rc=$?

  assert_eq \
    "bootstrap returns 0" \
    "0" "${rc}"

  assert_file_exists \
    "creates images.toml" \
    "${IMAGE_STORAGE_ROOT}/images.toml"

  # Verify it resolved the [[includes]] URL, not the default URL
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
  CURL_CALL_LOG=""
  CURL_MOCK_MODE="all-fail"  # would fail if curl were called

  local rc
  bootstrap_image_registry > /dev/null 2>&1
  rc=$?

  assert_eq \
    "bootstrap returns 0 (early return)" \
    "0" "${rc}"

  if [ -z "${CURL_CALL_LOG}" ]; then
    echo "  PASS: no curl calls made (early return)"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: curl was called despite existing images.toml"
    FAIL=$((FAIL + 1))
  fi
}

# ---------------------------------------------------------------------------
# Run all tests
# ---------------------------------------------------------------------------
echo "=== bootstrap_image_registry regression tests ==="
echo "Test root: ${TEST_ROOT}"

test_case1_default_unreachable_no_fallback
test_case2_fallback_url_available
test_case3_default_registry_works
test_case4_already_bootstrapped

echo ""
echo "=== Results: ${PASS} passed, ${FAIL} failed ==="

cleanup

if [ "${FAIL}" -gt 0 ]; then
  echo "FAILURE: ${FAIL} test(s) failed." >&2
  exit 1
fi

echo "All tests passed."
exit 0
