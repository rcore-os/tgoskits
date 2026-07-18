#!/bin/sh
# On-target driver for the POSIX message queue carpet.
#
# 1. Runs the deterministic self-written carpet (mq_carpet), which already
#    prints its own MQ_OK / TEST PASSED aggregate.
# 2. Runs every bundled LTP and Open POSIX conformance mq_* binary, counting
#    PASS/FAIL by exit status (0 = pass; Open POSIX uses 4/5 for
#    unsupported/untested, which are legitimate skips). PTS_UNRESOLVED (2) is
#    treated as a failure, not a skip, since it means the test errored out
#    before reaching a verdict.
#
# The final aggregate combines the carpet check count with the suite pass
# count and emits MQ_OK=<pass>/<total> and TEST PASSED only when nothing
# failed.

BIN=/usr/bin/mqueue-tests

pass=0
skip=0
total=0
fail_list=""

run_one() {
	name="$1"
	total=$((total + 1))
	out="$("$name" 2>&1)"
	rc=$?
	# Open POSIX return codes: PTS_PASS=0, PTS_FAIL=1, PTS_UNRESOLVED=2,
	# PTS_UNSUPPORTED=4, PTS_UNTESTED=5.
	#
	# PTS_FAIL (1) and PTS_UNRESOLVED (2) are BOTH real failures: FAIL is a
	# wrong result, and UNRESOLVED means the test hit an error and could not
	# reach a verdict - on a correct kernel it must not happen, so counting it
	# as a pass (or a silent skip) would hide a genuine regression. Only
	# PTS_UNSUPPORTED (4) and PTS_UNTESTED (5) are legitimate skips: the case
	# declares up front that it cannot run in this environment.
	if [ "$rc" -eq 0 ]; then
		pass=$((pass + 1))
		echo "ok - $(basename "$name")"
	elif [ "$rc" -eq 4 ] || [ "$rc" -eq 5 ]; then
		skip=$((skip + 1))
		total=$((total - 1))
		echo "skip - $(basename "$name") (rc=$rc)"
	else
		fail_list="$fail_list $(basename "$name")(rc=$rc)"
		echo "not ok - $(basename "$name") (rc=$rc)"
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

echo "=== LTP + Open POSIX conformance ==="
for t in "$BIN"/ltp_* "$BIN"/op_*; do
	[ -x "$t" ] || continue
	run_one "$t"
done

echo "SUITE PASS=$pass/$total SKIP=$skip"
if [ -n "$fail_list" ]; then
	echo "FAILED:$fail_list"
fi

# Gate: exactly 10 legitimate skips expected (the cases that require real-time
# thread scheduling or hardware features unavailable in this environment). A
# count higher than 10 indicates a systemic problem (e.g. a broken binary
# returning PTS_UNSUPPORTED instead of running). A count lower than 10 would
# mean a previously-expected skip now runs, which is fine but worth noticing.
if [ "$skip" -gt 10 ]; then
	echo "ERROR: $skip conformance cases skipped (expected <=10); possible systemic issue"
	echo "MQ_OK=$pass/$total"
	echo "TEST FAILED"
	exit 1
fi

if [ "$carpet_rc" -eq 0 ] && [ -z "$fail_list" ]; then
	echo "MQ_OK=$pass/$total"
	echo "TEST PASSED"
	exit 0
fi

echo "MQ_OK=$pass/$total"
echo "TEST FAILED"
exit 1
