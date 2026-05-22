#!/bin/sh
set -u

fail()
{
    echo "DIFFUTILS_TEST_FAILED: $*"
    exit 1
}

expect_status()
{
    expected="$1"
    shift

    "$@"
    status=$?
    if [ "$status" -ne "$expected" ]; then
        fail "$* exited $status, expected $expected"
    fi
}

echo "=== GNU diffutils app test ==="

apk update || fail "apk update"
apk add diffutils || fail "apk add diffutils"

command -v diff >/dev/null 2>&1 || fail "diff is missing"
diff --version | head -n 1 | grep -q "GNU diffutils" || fail "not GNU diffutils diff"

workdir="/tmp/starry-diffutils"
rm -rf "$workdir"
mkdir -p "$workdir/left/sub" "$workdir/right/sub" || fail "mkdir workdir"

printf "same\ncontent\n" > "$workdir/same-a" || fail "write same-a"
cp "$workdir/same-a" "$workdir/same-b" || fail "copy same-b"

expect_status 0 diff "$workdir/same-a" "$workdir/same-b"

printf "alpha\nbeta\n" > "$workdir/old.txt" || fail "write old.txt"
printf "alpha\ngamma\n" > "$workdir/new.txt" || fail "write new.txt"

diff -u "$workdir/old.txt" "$workdir/new.txt" > "$workdir/unified.patch" 2> "$workdir/unified.err"
status=$?
if [ "$status" -ne 1 ]; then
    fail "diff -u exited $status, expected 1"
fi
test ! -s "$workdir/unified.err" || fail "diff -u wrote stderr"
grep -q "^--- .*old.txt" "$workdir/unified.patch" || fail "missing unified old header"
grep -q "^+++ .*new.txt" "$workdir/unified.patch" || fail "missing unified new header"
grep -q "^-beta" "$workdir/unified.patch" || fail "missing removed line"
grep -q "^+gamma" "$workdir/unified.patch" || fail "missing added line"

diff "$workdir/old.txt" "$workdir/missing.txt" > "$workdir/missing.out" 2> "$workdir/missing.err"
status=$?
if [ "$status" -ne 2 ]; then
    fail "diff missing file exited $status, expected 2"
fi
grep -q "No such file" "$workdir/missing.err" || fail "missing ENOENT diagnostic"

printf "root\n" > "$workdir/left/root.txt" || fail "write left root"
printf "root\n" > "$workdir/right/root.txt" || fail "write right root"
printf "nested-left\n" > "$workdir/left/sub/nested.txt" || fail "write left nested"
printf "nested-right\n" > "$workdir/right/sub/nested.txt" || fail "write right nested"

diff -qr "$workdir/left" "$workdir/right" > "$workdir/recursive.out" 2> "$workdir/recursive.err"
status=$?
if [ "$status" -ne 1 ]; then
    fail "diff -qr exited $status, expected 1"
fi
test ! -s "$workdir/recursive.err" || fail "diff -qr wrote stderr"
grep -q "nested.txt differ" "$workdir/recursive.out" || fail "missing recursive diff output"

rm -rf "$workdir" || fail "cleanup"

echo "DIFFUTILS_TEST_PASSED"
