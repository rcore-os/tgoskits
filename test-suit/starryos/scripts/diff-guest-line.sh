#!/usr/bin/sh
# Compare one guest serial line to probes/expected/<probe>.line
# Usage:
#   echo 'CASE read_stdin_zero.zero_count ret=0 errno=0 note=handwritten' | \
#     ./diff-guest-line.sh read_stdin_zero
# Or pass line as second argument:
#   ./diff-guest-line.sh read_stdin_zero 'CASE read_stdin_zero...'
set -eu
PKG="$(cd "$(dirname "$0")/.." && pwd)"
probe="${1:?usage: $0 <probe_basename> [line]}"
exp="$PKG/probes/expected/${probe}.line"
test -f "$exp" || { echo "Missing $exp" >&2; exit 1; }

if [ "$#" -ge 2 ]; then
  got="$(printf '%s' "$2" | tr -d '\r')"
else
  got="$(cat)"
  got="$(printf '%s' "$got" | tr -d '\r')"
fi

want="$(cat "$exp")"
if [ "$got" != "$want" ]; then
  echo "DIFF guest vs oracle ($probe):" >&2
  echo "  want: $want" >&2
  echo "  got:  $got" >&2
  exit 1
fi
echo "OK: $probe matches oracle line"
