#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
temporary_directory=$(mktemp -d)
trap 'rm -rf -- "$temporary_directory"' EXIT

cc -std=c11 -Wall -Wextra -Werror -pedantic \
    "$script_dir/test_miss_accounting.c" \
    -o "$temporary_directory/test-miss-accounting"
"$temporary_directory/test-miss-accounting"
python3 -m unittest discover -s "$script_dir" -p 'test_*.py' -v
