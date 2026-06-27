#!/bin/sh
set -eu

fail() {
    echo "MAKE_TEST_FAILED: $*"
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "missing command: $1"
}

run() {
    echo "MAKE_CMD: $*"
    "$@" 2>&1
}

expect_output_contains() {
    file="$1"
    pattern="$2"
    desc="$3"
    grep -q "$pattern" "$file" || {
        cat "$file"
        fail "$desc"
    }
    echo "MAKE_CHECK_OK: $desc"
}

echo "MAKE_TEST_BEGIN"

need_cmd make

rm -rf /tmp/make-smoke-work /tmp/make-smoke-install
mkdir -p /tmp/make-smoke-work
cp -R /usr/src/make-smoke/. /tmp/make-smoke-work/
cd /tmp/make-smoke-work

run make clean
run make -j2 V=1
test -x ./make-smoke || fail "make did not create executable"

run ./make-smoke | tee /tmp/make-smoke-run.out
expect_output_contains /tmp/make-smoke-run.out "make-smoke: 42" \
    "built binary produces expected output"

run make test
run make install DESTDIR=/tmp/make-smoke-install
test -x /tmp/make-smoke-install/usr/bin/make-smoke || fail "make install did not install executable"

run /tmp/make-smoke-install/usr/bin/make-smoke | tee /tmp/make-smoke-install.out
expect_output_contains /tmp/make-smoke-install.out "make-smoke: 42" \
    "installed binary produces expected output"

run make clean
test ! -e ./make-smoke || fail "make clean did not remove executable"
test ! -d ./build || fail "make clean did not remove build directory"

echo "MAKE_TEST_PASSED"
