#!/bin/sh
set -eu

app_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
mode=${1:-smoke}
[ "$#" -gt 0 ] && shift
exec sh "$app_dir/runner/apache-runner.sh" "$mode" "$@"
