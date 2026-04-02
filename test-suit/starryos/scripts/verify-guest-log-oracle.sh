#!/usr/bin/sh
# From a QEMU/serial log (file or stdin), take the first line matching ^CASE  and
# compare it to probes/expected/<probe>.line (same as Linux oracle).
#
# Usage:
#   ./verify-guest-log-oracle.sh <probe_basename> [log_file]
#   ./verify-guest-log-oracle.sh <probe_basename> -     # stdin
#   ./verify-guest-log-oracle.sh write_stdout qemu.log
#
# Exit: 0 match, 1 mismatch, 2 no CASE line found in input
set -eu
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

probe="${1:?usage: $0 <probe_basename> [log_file|-]}"
shift

if [ "$#" -ge 1 ]; then
  log_arg="$1"
else
  log_arg="-"
fi

if [ "$log_arg" = "-" ]; then
  line="$("$SCRIPT_DIR/extract-case-line.sh")"
else
  test -f "$log_arg" || { echo "verify-guest-log-oracle: not a file: $log_arg" >&2; exit 2; }
  line="$("$SCRIPT_DIR/extract-case-line.sh" "$log_arg")"
fi

if [ -z "$line" ]; then
  echo "verify-guest-log-oracle: no line matching ^CASE  (probe=$probe)" >&2
  exit 2
fi

exec "$SCRIPT_DIR/diff-guest-line.sh" "$probe" "$line"
