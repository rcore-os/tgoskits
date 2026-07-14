#!/bin/sh
set -eu

suite_dir=${SYSCALL_SUITE_DIR:-/usr/share/starry-test-suit/syscall}
syscalls_dir="$suite_dir/syscalls"
todo_file="$suite_dir/TODO.txt"
run_file="$suite_dir/RUN.txt"
work_dir=/tmp/syscall
ltp_root=${LTPROOT:-/opt/ltp}
ltp_runtest_file="$ltp_root/runtest/syscalls"
ltp_bin_dir="$ltp_root/testcases/bin"

fail() {
    echo "SYSCALL_TEST_FAILED: $1"
    exit 1
}

ltp_fail() {
    echo "SYSCALL_CASE_FAILED: syscall=${SYSCALL_NAME:-unknown} reason=$1"
    exit 1
}

ltp_trim() {
    printf '%s' "$1" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//'
}

ltp_list_contains() {
    list_file=$1
    needle=$2
    [ -f "$list_file" ] || return 1

    while IFS= read -r line || [ -n "$line" ]; do
        line=${line%%#*}
        line=$(ltp_trim "$line")
        [ -n "$line" ] || continue
        [ "$line" = "$needle" ] && return 0
    done < "$list_file"

    return 1
}

ltp_prepare_env() {
    [ -d "$ltp_root" ] || ltp_fail "missing LTP root: $ltp_root"
    [ -f "$ltp_runtest_file" ] || ltp_fail "missing LTP syscalls runtest: $ltp_runtest_file"
    [ -d "$ltp_bin_dir" ] || ltp_fail "missing LTP syscall binaries: $ltp_bin_dir"

    export LTPROOT="$ltp_root"
    export PATH="$ltp_bin_dir:$PATH"
    export LTP_TIMEOUT_MUL="${LTP_TIMEOUT_MUL:-20}"
}

run_ltp_syscall_case() {
    test_bin=$1
    shift
    test_path="$ltp_bin_dir/$test_bin"

    [ -e "$test_path" ] || ltp_fail "missing LTP case: $test_bin"
    [ -x "$test_path" ] || ltp_fail "LTP case is not executable: $test_path"

    if [ "$#" -gt 0 ]; then
        echo "SYSCALL_CASE_BEGIN: syscall=${SYSCALL_NAME:-unknown} case=$test_bin args=$*"
    else
        echo "SYSCALL_CASE_BEGIN: syscall=${SYSCALL_NAME:-unknown} case=$test_bin"
    fi

    if command -v timeout >/dev/null 2>&1; then
        timeout "${LTP_CASE_TIMEOUT:-1800}" "$test_path" "$@" || ltp_fail "$test_bin exited with $?"
    else
        "$test_path" "$@" || ltp_fail "$test_bin exited with $?"
    fi
    echo "SYSCALL_CASE_PASSED: syscall=${SYSCALL_NAME:-unknown} case=$test_bin"
}

run_ltp_syscall_pattern() {
    spec=$1
    matched=0
    ran=0
    had_noglob=0

    case "$-" in
        *f*)
            had_noglob=1
            ;;
    esac

    set -f
    # shellcheck disable=SC2086
    set -- $spec
    if [ "$had_noglob" -eq 0 ]; then
        set +f
    fi

    pattern=$1
    shift

    echo "SYSCALL_PATTERN: $pattern"
    if [ "$#" -gt 0 ]; then
        echo "SYSCALL_ARGS: $*"
    fi

    for test_path in "$ltp_bin_dir"/$pattern; do
        [ -e "$test_path" ] || continue
        test_bin=$(basename "$test_path")
        matched=1
        ran=1
        if [ "$#" -gt 0 ]; then
            run_ltp_syscall_case "$test_bin" "$@"
        else
            run_ltp_syscall_case "$test_bin"
        fi
    done

    [ "$matched" -eq 1 ] || ltp_fail "no LTP cases matched pattern: $pattern"
    [ "$ran" -eq 1 ] || ltp_fail "no LTP cases ran for pattern: $pattern"
}

run_ltp_syscall_file() {
    syscall_file=$1
    listed=0
    runnable=0

    ltp_prepare_env

    echo "SYSCALL_RUNTEST: $ltp_runtest_file"
    echo "SYSCALL_BIN_DIR: $ltp_bin_dir"
    echo "SYSCALL_LIST: $syscall_file"

    while IFS= read -r line || [ -n "$line" ]; do
        line=$(ltp_trim "$line")
        [ -n "$line" ] || continue

        case "$line" in
            \#*)
                continue
                ;;
        esac

        line=${line%%#*}
        line=$(ltp_trim "$line")
        [ -n "$line" ] || continue

        listed=1
        runnable=1
        run_ltp_syscall_pattern "$line"
    done < "$syscall_file"

    if [ "$listed" -ne 1 ]; then
        echo "SYSCALL_SKIPPED: syscall=${SYSCALL_NAME:-unknown} reason=no-enabled-cases"
        return 0
    fi
    [ "$runnable" -eq 1 ] || echo "SYSCALL_SKIPPED: syscall=${SYSCALL_NAME:-unknown} reason=no-runnable-cases"
}

run_syscall_txt() {
    syscall=$1
    syscall_file=$2

    found=1
    if ltp_list_contains "$todo_file" "$syscall"; then
        echo "SYSCALL_SKIPPED: syscall=$syscall reason=todo"
        return 0
    fi

    SYSCALL_NAME=$syscall
    export SYSCALL_NAME

    echo "SYSCALL_BEGIN: $syscall"
    run_ltp_syscall_file "$syscall_file" || fail "syscall runner failed: $syscall"
    echo "SYSCALL_PASSED: $syscall"
}

validate_syscall_name() {
    syscall=$1
    case "$syscall" in
        *[!A-Za-z0-9_.-]* | "" | *".txt")
            fail "invalid syscall name: $syscall"
            ;;
    esac
}

run_requested_syscall() {
    syscall=$1

    validate_syscall_name "$syscall"
    if ltp_list_contains "$todo_file" "$syscall"; then
        found=1
        echo "SYSCALL_SKIPPED: syscall=$syscall reason=todo"
        return 0
    fi

    syscall_file="$syscalls_dir/$syscall.txt"
    [ -f "$syscall_file" ] || fail "unknown syscall: $syscall"
    run_syscall_txt "$syscall" "$syscall_file"
}

run_syscalls_from_run_file() {
    [ -f "$run_file" ] || return 1

    selected=0
    while IFS= read -r line || [ -n "$line" ]; do
        line=${line%%#*}
        line=$(ltp_trim "$line")
        [ -n "$line" ] || continue

        if [ "$selected" -eq 0 ]; then
            echo "SYSCALL_RUN_SELECTOR: $run_file"
        fi
        selected=1
        run_requested_syscall "$line"
    done < "$run_file"

    [ "$selected" -eq 1 ]
}

run_all_syscalls() {
    for syscall_file in "$syscalls_dir"/*.txt; do
        [ -f "$syscall_file" ] || continue
        syscall=$(basename "$syscall_file" .txt)
        run_syscall_txt "$syscall" "$syscall_file"
    done
}

[ -d "$suite_dir" ] || fail "missing syscall suite dir: $suite_dir"
[ -d "$syscalls_dir" ] || fail "missing syscall list dir: $syscalls_dir"

mkdir -p "$work_dir"
cd "$work_dir"

export TMPDIR="$work_dir"
export TST_TMPDIR="$work_dir"

found=0

echo "SYSCALL_TEST_BEGIN"
echo "SYSCALL_SUITE_DIR: $suite_dir"

if [ "$#" -eq 0 ]; then
    run_syscalls_from_run_file || run_all_syscalls
else
    for syscall in "$@"; do
        run_requested_syscall "$syscall"
    done
fi

[ "$found" -eq 1 ] || fail "no enabled syscall txt files found"

echo "SYSCALL_TEST_PASSED"
