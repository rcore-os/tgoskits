#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
compiler=${CC:-cc}
build_dir=$(mktemp -d)

cleanup() {
    rm -rf -- "$build_dir"
}
trap cleanup EXIT

"$compiler" -std=c11 -Wall -Wextra -Werror -pedantic \
    -I"$script_dir/src" \
    "$script_dir/src/protocol.c" \
    "$script_dir/src/endpoint.c" \
    "$script_dir/tests/host_logic.c" \
    -o "$build_dir/ivc-zephyr-host-tests"

"$build_dir/ivc-zephyr-host-tests"
