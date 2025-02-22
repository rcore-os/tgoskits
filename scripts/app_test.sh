#!/bin/bash

TIMEOUT=60s
EXIT_STATUS=0
ROOT=$(realpath $(dirname $0))/../
AX_ROOT=$ROOT/.arceos
S_PASS=0
S_FAILED=1
S_TIMEOUT=2
S_BUILD_FAILED=3

RED_C="\x1b[31;1m"
GREEN_C="\x1b[32;1m"
YELLOW_C="\x1b[33;1m"
CYAN_C="\x1b[36;1m"
BLOD_C="\x1b[1m"
END_C="\x1b[0m"

if [ -z "$ARCH" ]; then
    ARCH=x86_64
fi
if [ "$ARCH" != "x86_64" ] && [ "$ARCH" != "riscv64" ] && [ "$ARCH" != "aarch64" ] && [ "$ARCH" != "loongarch64" ]; then
    echo "Unknown architecture: $ARCH"
    exit $S_FAILED
fi


function compare() {
    local actual=$1
    local expect=$2
    if [ ! -f "$expect" ]; then
        MSG="expected output file \"${BLOD_C}$expect${END_C}\" not found!"
        return $S_FAILED
    fi
    IFS=''
    while read -r line; do
        local matched=$(grep -m1 -a "$line" < "$actual")
        if [ -z "$matched" ]; then
            MSG="pattern \"${BLOD_C}$line${END_C}\" not matched!"
            unset IFS
            return $S_FAILED
        fi
    done < "$expect"
    unset IFS
    return $S_PASS
}

function run_and_compare() {
    local args=$1
    local expect=$2
    local actual=$3

    echo -ne "    run with \"${BLOD_C}$args${END_C}\": "

    make -C "$ROOT" AX_TESTCASE=$APP $args build > "$actual" 2>&1
    if [ $? -ne 0 ]; then
        return $S_BUILD_FAILED
    fi

    TIMEFORMAT='%3Rs'
    RUN_TIME=$( { time { timeout --foreground $TIMEOUT make -C "$ROOT" AX_TESTCASE=$APP $args justrun > "$actual" 2>&1; }; } 2>&1 )
    local res=$?
    if [ $res == 124 ]; then
        return $S_TIMEOUT
    elif [ $res -ne 0 ]; then
        return $S_FAILED
    fi

    compare "$actual" "$expect"
    if [ $? -ne 0 ]; then
        return $S_FAILED
    else
        return $S_PASS
    fi
}


function test_one() {
    local args=$1
    local expect="$APP_DIR/$2"
    local actual="$APP_DIR/actual.out"
    local config_file=$(realpath --relative-to=$AX_ROOT "$ROOT/configs/$ARCH.toml")
    args="$args ARCH=$ARCH ACCEL=n EXTRA_CONFIG=$config_file"
    rm -f "$actual"

    MSG=
    run_and_compare "$args" "$expect" "$actual"
    local res=$?

    if [ $res -ne $S_PASS ]; then
        EXIT_STATUS=$res
        if [ $res == $S_FAILED ]; then
            echo -e "${RED_C}failed!${END_C} $RUN_TIME"
        elif [ $res == $S_TIMEOUT ]; then
            echo -e "${YELLOW_C}timeout!${END_C} $RUN_TIME"
        elif [ $res == $S_BUILD_FAILED ]; then
            echo -e "${RED_C}build failed!${END_C}"
        fi
        if [ ! -z "$MSG" ]; then
            echo -e "        $MSG"
        fi
        echo -e "${RED_C}actual output${END_C}:"
        cat "$actual"
    else
        echo -e "${GREEN_C}passed!${END_C} $RUN_TIME"
        rm -f "$actual"
    fi
}

# TODO: add more testcases
test_list=(
    "nimbos"
    "libc"
)

for t in ${test_list[@]}; do
    APP=$t
    APP_DIR=$(realpath "$(pwd)/apps/$t")
    make -C "$ROOT" user_apps AX_TESTCASE=$t
    echo -e "${CYAN_C}Testing${END_C} $t:"
    source "$APP_DIR/test_cmd"
done

echo -e "test script exited with: $EXIT_STATUS"
exit $EXIT_STATUS
