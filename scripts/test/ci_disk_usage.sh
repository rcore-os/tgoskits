#!/usr/bin/env bash

set -uo pipefail

printf 'Disk usage snapshot: %s\n' "${1:-unspecified}"
date -u '+%Y-%m-%dT%H:%M:%SZ'
df -B1 "${GITHUB_WORKSPACE:-.}" /tmp
df -i "${GITHUB_WORKSPACE:-.}" /tmp

for directory in \
  "${GITHUB_WORKSPACE:-.}/target" \
  /tmp/tgosimages \
  /tmp/ostool
do
  if [ -d "${directory}" ]; then
    du -B1 --max-depth=2 "${directory}" || true
  fi
done
