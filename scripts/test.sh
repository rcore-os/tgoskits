#!/bin/bash
# StarryOS test script – mirrors the structure of app-helloworld/scripts/test.sh.
#
# Usage:
#   ./scripts/test.sh [STEP]
#
# STEP (optional): run only the specified step (1–5). Omit to run all steps.
#   1  – check required tools
#   2  – code format check  (cargo fmt --all)
#   3  – per-architecture rootfs download + kernel build + QEMU boot test
#   4  – publish readiness check (cargo publish --dry-run)
#   5  – print test summary
#
# Environment variables:
#   SKIP_QEMU=true   skip QEMU boot tests in step 3 (build only)
#
# Examples:
#   ./scripts/test.sh          # run all five steps
#   ./scripts/test.sh 1        # tool check only
#   ./scripts/test.sh 3        # build + QEMU boot tests only
#   ./scripts/test.sh 4        # publish dry-run only
#   SKIP_QEMU=true ./scripts/test.sh 3   # build only, no QEMU

ARCHS=("riscv64" "aarch64" "loongarch64" "x86_64")

# --------------------------------------------------------------------------
# Step 1 – Check required tools
# --------------------------------------------------------------------------
step1_check_tools() {
    echo "[1/5] Checking required tools..."

    local missing=false

    for tool in make cargo python3; do
        if ! command -v "$tool" &> /dev/null; then
            echo "  ERROR: '$tool' is not installed"
            missing=true
        fi
    done

    for arch in "${ARCHS[@]}"; do
        if ! command -v "qemu-system-${arch}" &> /dev/null; then
            echo "  WARNING: 'qemu-system-${arch}' not found (QEMU test for ${arch} will be skipped)"
        fi
    done

    if [ "$missing" = "true" ]; then
        echo "ERROR: missing required tools – aborting."
        exit 1
    fi

    echo "  OK: all required tools are available"
    echo ""
}

# --------------------------------------------------------------------------
# Step 2 – Code format check
# --------------------------------------------------------------------------
step2_check_format() {
    echo "[2/5] Checking code format..."
    cargo fmt --all -- --check
    echo "  OK: code format check passed"
    echo ""
}

# --------------------------------------------------------------------------
# Step 3 – Per-architecture: rootfs + build + QEMU boot test
# --------------------------------------------------------------------------
step3_arch_tests() {
    local skip_qemu="${SKIP_QEMU:-false}"
    echo "[3/5] Running architecture-specific build and boot tests..."
    if [ "$skip_qemu" = "true" ]; then
        echo "  NOTE: SKIP_QEMU=true – QEMU boot tests will be skipped"
    fi

    local all_passed=true

    for arch in "${ARCHS[@]}"; do
        echo ""
        echo "  --- Architecture: ${arch} ---"

        # Rootfs download (skipped by Makefile if image already exists)
        echo "  [${arch}] Downloading rootfs..."
        make ARCH="${arch}" rootfs

        # Kernel build
        echo "  [${arch}] Building kernel..."
        make ARCH="${arch}" build

        # QEMU boot test
        if [ "$skip_qemu" = "true" ]; then
            echo "  [${arch}] Skipping QEMU boot test (SKIP_QEMU=true)"
            continue
        fi

        if ! command -v "qemu-system-${arch}" &> /dev/null; then
            echo "  [${arch}] WARNING: qemu-system-${arch} not found, skipping boot test"
            continue
        fi

        echo "  [${arch}] Running QEMU boot test..."
        if make ARCH="${arch}" ci-test; then
            echo "  [${arch}] OK: boot test passed"
        else
            echo "  [${arch}] ERROR: boot test FAILED"
            all_passed=false
        fi
    done

    echo ""
    if [ "$all_passed" = "true" ]; then
        echo "  OK: all architecture tests passed"
    else
        echo "ERROR: one or more architecture tests failed"
        exit 1
    fi
    echo ""
}

# --------------------------------------------------------------------------
# Step 4 – Publish readiness check
# --------------------------------------------------------------------------
step4_publish() {
    echo "[4/5] Checking publish readiness (cargo publish --dry-run)..."

    local all_passed=true

    echo ""
    echo "  --- Publish check ---"
    if cargo publish --workspace --dry-run --allow-dirty; then
        echo "  [${arch}] OK: publish check passed"
    else
        echo "  [${arch}] ERROR: publish check FAILED"
        all_passed=false
    fi

    echo ""
    if [ "$all_passed" = "true" ]; then
        echo "  OK: publish check passed"
    else
        echo "ERROR: publish check failed"
        exit 1
    fi
    echo ""
}

# --------------------------------------------------------------------------
# Step 5 – Summary
# --------------------------------------------------------------------------
step5_summary() {
    echo "[5/5] Test Summary"
    echo "  =============================="
    echo "  OK: all requested steps completed successfully!"
    echo ""
    echo "  Steps available in this script:"
    echo "    1 – tool availability check  (make, cargo, python3, qemu-system-*)"
    echo "    2 – code format check        (cargo fmt --all)"
    echo "    3 – per-arch build + boot    (riscv64, aarch64, loongarch64, x86_64)"
    echo "    4 – publish readiness check  (cargo publish --dry-run)"
    echo "    5 – summary"
    echo ""
}

# --------------------------------------------------------------------------
# Usage / help  (prints only the leading comment block of this file)
# --------------------------------------------------------------------------
usage() {
    # Print lines 2..N of this file that start with '#', stop at first blank
    # or non-comment line – those lines form the usage documentation block.
    tail -n +2 "$0" | while IFS= read -r line; do
        case "$line" in
            '#'*)  printf '%s\n' "${line}" | sed 's/^#[[:space:]]*//' ;;
            *)     break ;;
        esac
    done
    exit 0
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------
main() {
    local step="${1:-all}"

    case "$step" in
        -h|--help) usage ;;
        all)
            echo "=== StarryOS Test Script (all steps) ==="
            echo ""
            step1_check_tools
            step2_check_format
            step3_arch_tests
            step4_publish
            step5_summary
            ;;
        1)
            echo "=== StarryOS Test Script (step 1: tool check) ==="
            echo ""
            step1_check_tools
            ;;
        2)
            echo "=== StarryOS Test Script (step 2: format check) ==="
            echo ""
            step2_check_format
            ;;
        3)
            echo "=== StarryOS Test Script (step 3: arch build + boot tests) ==="
            echo ""
            step3_arch_tests
            ;;
        4)
            echo "=== StarryOS Test Script (step 4: publish readiness check) ==="
            echo ""
            step4_publish
            ;;
        5)
            echo "=== StarryOS Test Script (step 5: summary) ==="
            echo ""
            step5_summary
            ;;
        *)
            echo "ERROR: unknown step '${step}'. Valid values: 1, 2, 3, 4, 5, or omit for all."
            echo "Run '$0 --help' for usage."
            exit 1
            ;;
    esac
}

main "$@"
