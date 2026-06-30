#!/bin/sh
fail=0
checked=0
fail_marker=STARRY_TTY_INPUT_BURST_""FAILED

check() {
    name="$1"
    got="$2"
    want="$3"
    checked=$((checked + 1))
    if [ "$got" = "$want" ]; then
        echo "STARRY_TTY_INPUT_BURST_OK:$name:${#got}"
    else
        echo "$fail_marker:$name:got=${#got}:want=${#want}:$got"
        fail=1
    fi
}

check short "abcdefghijklmnopqrstuvwxyz" "abcdefghijklmnopqrstuvwxyz"
check digits "012345678901234567890123456789012345678901234567890123456789" "012345678901234567890123456789012345678901234567890123456789"
check cmd01 "qwertyuiopasdfghjklzxcvbnm" "qwertyuiopasdfghjklzxcvbnm"
check cmd02 "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZ" "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZ"
check long01 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
check long02 "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
check long03 "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC" "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"
check mix01 "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-_.:/0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-END" "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-_.:/0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-END"

i=0
while [ "$i" -lt 120 ]; do
    i=$((i + 1))
    got="line-${i}-abcdefghijklmnopqrstuvwxyz-0123456789-ABCDEFGHIJKLMNOPQRSTUVWXYZ-tty-input-stress-END"
    want=$(printf 'line-%s-%s-%s-%s-%s' "$i" "abcdefghijklmnopqrstuvwxyz" "0123456789" "ABCDEFGHIJKLMNOPQRSTUVWXYZ" "tty-input-stress-END")
    check "loop${i}" "$got" "$want"
done

if [ "$checked" -ne 128 ]; then
    echo "$fail_marker:checked-count:$checked"
    fail=1
fi

if [ "$fail" -eq 0 ]; then
    echo STARRY_TTY_INPUT_BURST_PASSED
else
    echo "$fail_marker"
fi
