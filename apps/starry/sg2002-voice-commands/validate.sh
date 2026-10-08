#!/bin/sh
set -eu
cd -- "$(dirname -- "$0")"
temporary=$(mktemp -d)
trap 'rm -f "$temporary/commands" "$temporary/noncommands" "$temporary/repeated"; rmdir "$temporary"' EXIT
failed() { echo VOICE_COMMANDS_TEST_FAILED; exit 1; }
# Match the complete ordered event stream, not a substring in model diagnostics.
check_events() {
    awk -v count="$2" '
    BEGIN { split("forward backward left right stop", expected); n = 0 }
    { n++; if ($0 !~ ("^\\{\"command\":\"" expected[(n - 1) % 5 + 1] "\",\"time\":[0-9]+[.][0-9]+\\}$")) exit 1 }
    END { if (n != count) exit 1 }
    ' "$1"
}
./run.sh validation/commands.wav > "$temporary/commands" || failed
check_events "$temporary/commands" 5 || failed
./run.sh validation/noncommands.wav > "$temporary/noncommands" || failed
[ ! -s "$temporary/noncommands" ] || failed
./run.sh --raw-file validation/repeated.pcm > "$temporary/repeated" || failed
check_events "$temporary/repeated" 10 || failed
cat "$temporary/commands"
cat "$temporary/repeated"
echo VOICE_COMMANDS_TEST_PASSED
