#!/bin/sh
exec 0</dev/null   # carpet never reads stdin; some b4 subcommands block on a tty otherwise
# b4 carpet for StarryOS.
#
# b4 (kernel.org contributor tool) is built around lore.kernel.org: its
# thread-fetch / am / shazam / send workflows require network + a live archive,
# which is out of scope for a deterministic offline carpet. This carpet instead
# exercises b4's fully-offline surface exhaustively: version, top-level help, the
# complete subcommand set, and each subcommand's own --help (usage + key flags).
# Every check is a HARD ASSERTION on real output, not "ran ok".
#
# Three-gate: emits B4_TEST_PASSED only when fail==0 && total==EXPECTED &&
# pass==EXPECTED. Network-dependent workflows (mbox/am/pr fetch from lore) are
# deliberately not asserted here; they belong to an on-target networked run.

set -u
LC_ALL=C; export LC_ALL
B4="b4"
command -v b4 >/dev/null 2>&1 || B4="python3 -m b4"

PASS=0; FAIL=0
pass() { PASS=$((PASS+1)); }
fail() { FAIL=$((FAIL+1)); echo "FAIL[$((PASS+FAIL))]: $1"; }

# assert_ok <label> <cmd...> : command exits 0
assert_ok() { label=$1; shift; if "$@" >/dev/null 2>&1; then pass; else fail "$label (exit $?)"; fi; }
# assert_contains <label> <needle> <haystack>
assert_contains() { case "$3" in *"$2"*) pass;; *) fail "$1: [$2] not in output";; esac; }
# assert_semver <label> <string>
assert_semver() { case "$2" in [0-9]*.[0-9]*.[0-9]*) pass;; *) fail "$1: [$2] not semver";; esac; }
# assert_help <subcmd> : `b4 <subcmd> --help` exits 0 and prints a usage line naming it
assert_help() {
    out=$($B4 "$1" --help 2>&1); rc=$?
    if [ $rc -eq 0 ] && { case "$out" in *usage:*"$1"*) true;; *) false;; esac; }; then pass
    else fail "help.$1: no usage line (rc=$rc)"; fi
}

# --- version (semver) ---
VER=$($B4 --version 2>&1 | tr -d '\r' | head -1 | awk '{print $NF}')
assert_semver "version" "$VER"
assert_ok "version.exit" $B4 --version

# --- top-level help lists every subcommand ---
HELP=$($B4 --help 2>&1)
assert_contains "help.exit" "usage:" "$HELP"
for sc in mbox am shazam pr diff ty prep send trailers kr; do
    assert_contains "help.lists.$sc" "$sc" "$HELP"
done

# --- each subcommand exposes its own --help (usage + subcmd name) ---
for sc in mbox am shazam pr diff ty prep send trailers kr; do
    assert_help "$sc"
done

# --- key offline-relevant flags are documented ---
assert_contains "mbox.local-flag"  "use-local-mbox" "$($B4 mbox --help 2>&1)"
assert_contains "am.outdir-flag"   "-o"             "$($B4 am --help 2>&1)"
assert_contains "trailers.update"  "-u"             "$($B4 trailers --help 2>&1)"

# --- unknown subcommand is rejected (negative control) ---
if $B4 no-such-subcmd >/dev/null 2>&1; then fail "neg.unknown-subcmd accepted"; else pass; fi

TOTAL=$((PASS+FAIL))
EXPECTED=27
echo "=== b4 carpet summary: PASS=$PASS FAIL=$FAIL TOTAL=$TOTAL EXPECTED=$EXPECTED ==="
if [ "$FAIL" -eq 0 ] && [ "$TOTAL" -eq "$EXPECTED" ] && [ "$PASS" -eq "$EXPECTED" ]; then
    echo "B4_TEST_PASSED"; exit 0
else
    echo "B4_TEST_FAILED"; exit 1
fi
