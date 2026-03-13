#!/bin/bash
# Run all integration tests for crate_interface.
#
# This script runs test binaries for both simple traits (no default implementations)
# and weak_default traits (with default implementations via weak symbols).
#
# Usage:
#   ./run_tests.sh           # Run all tests
#   ./run_tests.sh simple    # Run only simple tests (stable Rust)
#   ./run_tests.sh weak      # Run only weak_default tests (nightly Rust)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo_status() {
    echo -e "${GREEN}==>${NC} $1"
}

echo_warning() {
    echo -e "${YELLOW}WARNING:${NC} $1"
}

echo_error() {
    echo -e "${RED}ERROR:${NC} $1"
}

run_simple_tests() {
    echo_status "Running simple trait tests (stable Rust compatible)..."

    echo_status "  Building and running test-simple..."
    cargo run --bin test-simple

    echo_status "Simple trait tests passed!"
}

run_weak_tests() {
    echo_status "Running weak_default trait tests (requires nightly Rust)..."

    # Check if we're on nightly
    if ! rustc --version | grep -q nightly; then
        echo_warning "Not on nightly Rust. Attempting to use +nightly..."
        CARGO_CMD="cargo +nightly"
    else
        CARGO_CMD="cargo"
    fi

    echo_status "  Building and running test-weak (full implementation)..."
    $CARGO_CMD run --bin test-weak

    echo_status "  Building and running test-weak-partial (partial implementation)..."
    $CARGO_CMD run --bin test-weak-partial

    echo_status "Weak_default trait tests passed!"
}

run_all_tests() {
    run_simple_tests
    echo ""
    run_weak_tests
}

# Parse arguments
case "${1:-all}" in
    simple)
        run_simple_tests
        ;;
    weak)
        run_weak_tests
        ;;
    all)
        run_all_tests
        ;;
    *)
        echo "Usage: $0 [simple|weak|all]"
        echo "  simple  - Run only simple tests (stable Rust)"
        echo "  weak    - Run only weak_default tests (nightly Rust)"
        echo "  all     - Run all tests (default)"
        exit 1
        ;;
esac

echo ""
echo_status "All requested tests completed successfully!"
