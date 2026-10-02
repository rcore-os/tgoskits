#!/bin/bash
set -euo pipefail

run_dir=$1
target_dir=$2
marker=$3

mkdir -p -- "${run_dir%/*}" "${target_dir%/*}"
if ! mkdir -- "$run_dir"; then
    printf '===%s-FAIL reason=run-directory-already-exists===\n' "$marker"
    exit 1
fi
if ! mkdir -- "$target_dir"; then
    printf '===%s-FAIL reason=target-directory-already-exists===\n' "$marker"
    exit 1
fi
