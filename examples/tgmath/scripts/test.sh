#!/bin/bash
# Test script for tgmath component demo
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPONENT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$COMPONENT_DIR"

echo "Running tests..."
cargo test

echo "All tests passed."
