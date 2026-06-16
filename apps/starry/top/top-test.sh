#!/bin/sh
echo "=== install procps ==="
apk add procps || { echo "TOP_TEST_FAILED"; exit 1; }

echo "=== test top requires TERM ==="
output="/tmp/top_output.txt"
top -n 1 > "$output" 2> /dev/null

if [ -s "$output" ]; then
	rm -f "$output"
	echo "TOP_TEST_PASSED"
else
	rm -f "$output"
	echo "TOP_TEST_FAILED: top produced no output (TERM may not be set)"
	exit 1
fi
