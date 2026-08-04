#!/bin/sh
# On-target driver for the POSIX message queue carpet.
#
# 1. Runs the deterministic self-written carpet (mq_carpet), which already
#    prints its own MQ_OK / TEST PASSED aggregate.
# 2. Runs every bundled Open POSIX conformance mq_* binary (op_*), classifying
#    each by exit status.
#
# Open POSIX return codes: PTS_PASS=0, PTS_FAIL=1, PTS_UNRESOLVED=2,
# PTS_UNSUPPORTED=4, PTS_UNTESTED=5.
#
# Completeness gate (no-skip bar): the overlay ships exactly EXPECTED_OPENPOSIX
# conformance cases and StarryOS is expected to run every one of them to a PASS.
# The suite is green ONLY when all EXPECTED_OPENPOSIX cases are present and pass,
# with zero failures AND zero skips. PTS_FAIL/PTS_UNRESOLVED are failures (wrong
# result / errored before a verdict). PTS_UNSUPPORTED/PTS_UNTESTED are counted as
# skips and are ALSO treated as suite failures here: a skip means the kernel
# could not run a bundled case, i.e. a real gap - it is surfaced and fails the
# run rather than being waved through, so a silently-shrunk suite cannot read as
# a smaller-but-green PASS.

BIN=/usr/bin/mqueue-tests
EXPECTED_OPENPOSIX=119

pass=0
op_present=0
skip_list=""
fail_list=""

run_one() {
	name="$1"
	out="$("$name" 2>&1)"
	rc=$?
	base=$(basename "$name")
	if [ "$rc" -eq 0 ]; then
		pass=$((pass + 1))
		echo "ok - $base"
	elif [ "$rc" -eq 4 ] || [ "$rc" -eq 5 ]; then
		skip_list="$skip_list $base(rc=$rc)"
		echo "skip - $base (rc=$rc)"
	else
		fail_list="$fail_list $base(rc=$rc)"
		echo "not ok - $base (rc=$rc)"
		echo "$out" | sed 's/^/    /'
	fi
}

echo "=== self-written carpet ==="
carpet_rc=0
if [ -x "$BIN/mq_carpet" ]; then
	"$BIN/mq_carpet" || carpet_rc=1
else
	echo "not ok - mq_carpet missing"
	carpet_rc=1
fi

echo "=== Open POSIX conformance ==="
for t in "$BIN"/op_*; do
	[ -x "$t" ] || continue
	op_present=$((op_present + 1))
	run_one "$t"
done

echo "SUITE PASS=$pass/$EXPECTED_OPENPOSIX PRESENT=$op_present SKIP=${skip_list:-none}"
[ -n "$fail_list" ] && echo "FAILED:$fail_list"
[ -n "$skip_list" ] && echo "UNEXPECTED_SKIPS:$skip_list"
[ "$op_present" -ne "$EXPECTED_OPENPOSIX" ] && echo "COUNT_MISMATCH: present=$op_present expected=$EXPECTED_OPENPOSIX"

if [ "$carpet_rc" -eq 0 ] \
	&& [ -z "$fail_list" ] \
	&& [ -z "$skip_list" ] \
	&& [ "$op_present" -eq "$EXPECTED_OPENPOSIX" ] \
	&& [ "$pass" -eq "$EXPECTED_OPENPOSIX" ]; then
	echo "MQ_OK=$pass/$EXPECTED_OPENPOSIX"
	echo "TEST PASSED"
	exit 0
fi

echo "MQ_OK=$pass/$EXPECTED_OPENPOSIX"
echo "TEST FAILED"
exit 1
