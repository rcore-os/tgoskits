#!/bin/bash
# Check script for tgmath component demo
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPONENT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$COMPONENT_DIR"

echo "Running format check..."
cargo fmt -- --check

echo "Running clippy..."
cargo clippy -- -D warnings

echo "All checks passed."
