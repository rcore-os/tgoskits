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
command -v cmp >/dev/null 2>&1 || fail "cmp is missing"
command -v sdiff >/dev/null 2>&1 || fail "sdiff is missing"

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

printf "alpha\n\n beta \n" > "$workdir/space-a" || fail "write space-a"
printf "alpha\nbeta\n" > "$workdir/space-b" || fail "write space-b"
expect_status 0 diff -B -w "$workdir/space-a" "$workdir/space-b"

printf "crlf\r\nline\r\n" > "$workdir/crlf.txt" || fail "write crlf"
printf "crlf\nline\n" > "$workdir/lf.txt" || fail "write lf"
expect_status 0 diff --strip-trailing-cr "$workdir/crlf.txt" "$workdir/lf.txt"

diff "$workdir/old.txt" "$workdir/missing.txt" > "$workdir/missing.out" 2> "$workdir/missing.err"
status=$?
if [ "$status" -ne 2 ]; then
    fail "diff missing file exited $status, expected 2"
fi
grep -q "No such file" "$workdir/missing.err" || fail "missing ENOENT diagnostic"

printf "\001\002\003\004" > "$workdir/bin-a" || fail "write bin-a"
printf "\001\002\003\005" > "$workdir/bin-b" || fail "write bin-b"
diff "$workdir/bin-a" "$workdir/bin-b" > "$workdir/binary.out" 2> "$workdir/binary.err"
status=$?
if [ "$status" -ne 1 ]; then
    fail "binary diff exited $status, expected 1"
fi
test -s "$workdir/binary.out" || fail "binary diff produced no output"
test ! -s "$workdir/binary.err" || fail "binary diff wrote stderr"

printf "root\n" > "$workdir/left/root.txt" || fail "write left root"
printf "root\n" > "$workdir/right/root.txt" || fail "write right root"
printf "nested-left\n" > "$workdir/left/sub/nested.txt" || fail "write left nested"
printf "nested-right\n" > "$workdir/right/sub/nested.txt" || fail "write right nested"
printf "removed\n" > "$workdir/left/removed.txt" || fail "write removed"
printf "added\n" > "$workdir/right/added.txt" || fail "write added"

diff -qr "$workdir/left" "$workdir/right" > "$workdir/recursive.out" 2> "$workdir/recursive.err"
status=$?
if [ "$status" -ne 1 ]; then
    fail "diff -qr exited $status, expected 1"
fi
test ! -s "$workdir/recursive.err" || fail "diff -qr wrote stderr"
grep -q "nested.txt differ" "$workdir/recursive.out" || fail "missing recursive diff output"

diff -Naur "$workdir/left" "$workdir/right" > "$workdir/recursive.patch" 2> "$workdir/recursive.patch.err"
status=$?
if [ "$status" -ne 1 ]; then
    fail "diff -Naur exited $status, expected 1"
fi
test ! -s "$workdir/recursive.patch.err" || fail "diff -Naur wrote stderr"
grep -q "^+added" "$workdir/recursive.patch" || fail "missing added-file patch hunk"
grep -q "^-removed" "$workdir/recursive.patch" || fail "missing removed-file patch hunk"
grep -q "^-nested-left" "$workdir/recursive.patch" || fail "missing nested removed hunk"
grep -q "^+nested-right" "$workdir/recursive.patch" || fail "missing nested added hunk"

expect_status 0 cmp -s "$workdir/same-a" "$workdir/same-b"
expect_status 1 cmp -s "$workdir/old.txt" "$workdir/new.txt"
expect_status 2 cmp -s "$workdir/old.txt" "$workdir/no-such-cmp"

cmp -l "$workdir/bin-a" "$workdir/bin-b" > "$workdir/cmp-list.out" 2> "$workdir/cmp-list.err"
status=$?
if [ "$status" -ne 1 ]; then
    fail "cmp -l exited $status, expected 1"
fi
test -s "$workdir/cmp-list.out" || fail "cmp -l produced no output"
test ! -s "$workdir/cmp-list.err" || fail "cmp -l wrote stderr"

sdiff -s "$workdir/old.txt" "$workdir/new.txt" > "$workdir/sdiff.out" 2> "$workdir/sdiff.err"
status=$?
if [ "$status" -ne 1 ]; then
    fail "sdiff -s exited $status, expected 1"
fi
test ! -s "$workdir/sdiff.err" || fail "sdiff -s wrote stderr"
grep -q "beta.*gamma" "$workdir/sdiff.out" || fail "missing sdiff side-by-side output"

rm -rf "$workdir" || fail "cleanup"

echo "DIFFUTILS_TEST_PASSED"
