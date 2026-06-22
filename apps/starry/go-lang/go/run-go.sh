#!/bin/sh
# run-go.sh — on-target gate for the StarryOS go-lang carpet (#764).
#
# Staged into the rootfs by prebuild.sh and invoked by every qemu-<arch>.toml as the
# ENTIRE shell_init_cmd (`sh /usr/local/bin/run-go.sh`). Keeping the gate logic in a
# staged script (not inline in shell_init_cmd) is deliberate: the StarryOS app harness
# echoes the shell_init_cmd text back over the serial console, and an inline
# `echo "TEST PASSED"` would land — verbatim — in the captured stream and be matched by
# the harness `success_regex = (?m)^TEST PASSED$` as a FALSE POSITIVE (it would "pass"
# even when the real gate prints TEST FAILED). With the gate staged, the only echoed text
# is `sh /usr/local/bin/run-go.sh`, so the regex only ever matches this script's REAL
# stdout. (node-lang/run_node_carpet.sh uses the same pattern.)
#
# The carpet is the fully-static go1.26 binary /usr/local/bin/golang-lang (CGO_ENABLED=0,
# no libc/interp); its output is 100% deterministic, so the gate asserts it byte-for-byte
# against the host-generated golden (/root/golang-lang-golden.txt) AND requires the
# GO_LANG_OK token. TEST PASSED is printed ONLY here, ONLY when both hold.
set -u

GOT=/tmp/got.txt
GOLDEN=/root/golang-lang-golden.txt

# go-zero probes cgroup cpuset.cpus at import (CPU-quota detection); on a kernel without
# cgroup cpuset (StarryOS) it emits a structured JSON diagnostic to stdout carrying a
# non-deterministic @timestamp. Those JSON log lines are incidental go-zero diagnostics —
# never part of the carpet's chk() assertions — so filter them out to keep the output
# byte-exact vs the host golden (the host has cgroup, so it never emits them).
/usr/local/bin/golang-lang 2>&1 | grep -v '^{"@timestamp"' | sed 's/[[:space:]]*$//' > "$GOT"

if grep -q '^GO_LANG_OK' "$GOT" && cmp -s "$GOT" "$GOLDEN"; then
  echo "TEST PASSED"
  exit 0
fi

echo "go-lang: golang-lang output did not match golden"
diff "$GOLDEN" "$GOT" | head -20
echo "TEST FAILED"
exit 1
