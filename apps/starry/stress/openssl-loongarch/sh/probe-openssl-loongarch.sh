#!/bin/sh
PASS=0
FAIL=0

section() {
    echo
    echo "=== $1 ==="
}

pass() {
    echo "PASS: $1"
    PASS=$((PASS + 1))
}

fail() {
    echo "FAIL: $1 rc=$2"
    FAIL=$((FAIL + 1))
}

run_step() {
    name="$1"
    shift
    section "$name"
    "$@"
    rc=$?
    echo "RESULT: $name rc=$rc"
    if [ "$rc" -eq 0 ]; then
        pass "$name"
    else
        fail "$name" "$rc"
    fi
}

run_shell_step() {
    name="$1"
    script="$2"
    section "$name"
    sh -c "$script"
    rc=$?
    echo "RESULT: $name rc=$rc"
    if [ "$rc" -eq 0 ]; then
        pass "$name"
    else
        fail "$name" "$rc"
    fi
}

rm -rf /tmp/openssl-la
mkdir -p /tmp/openssl-la

run_step "cat cpuinfo" cat /proc/cpuinfo
run_shell_step "cpuinfo has lasx" "grep -q 'Feat.* lasx' /proc/cpuinfo"
section "auxv hwcap"
python3 - <<'PY'
import struct

data = open("/proc/self/auxv", "rb").read()
hwcap = None
for index in range(0, len(data), 16):
    key, value = struct.unpack_from("QQ", data, index)
    if key == 0:
        break
    if key == 16:
        hwcap = value
        break

names = [
    (1 << 0, "cpucfg"),
    (1 << 1, "lam"),
    (1 << 2, "ual"),
    (1 << 3, "fpu"),
    (1 << 4, "lsx"),
    (1 << 5, "lasx"),
    (1 << 6, "crc32"),
    (1 << 7, "complex"),
    (1 << 8, "crypto"),
    (1 << 9, "lvz"),
]
print("AT_HWCAP=0x%x" % (hwcap or 0))
print("AT_HWCAP_NAMES=%s" % " ".join(name for bit, name in names if hwcap and hwcap & bit))
required = (1 << 3) | (1 << 4) | (1 << 5)
if hwcap is None or hwcap & required != required:
    raise SystemExit("missing required fpu/lsx/lasx HWCAP bits")
PY
rc=$?
echo "RESULT: auxv hwcap rc=$rc"
if [ "$rc" -eq 0 ]; then
    pass "auxv hwcap"
else
    fail "auxv hwcap" "$rc"
fi

run_step "openssl version" openssl version -a
run_step "openssl rand" openssl rand -hex 16
run_step "openssl genrsa" openssl genrsa -out /tmp/openssl-la/genrsa-key.pem 2048
run_step "openssl req self-signed" openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=127.0.0.1 -keyout /tmp/openssl-la/req-key.pem -out /tmp/openssl-la/cert.pem
run_shell_step "python import ssl" "python3 -c 'import ssl; print(ssl.OPENSSL_VERSION)'"
run_shell_step "python ssl context" "python3 -c 'import ssl; ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); print(\"context ok\")'"

echo
echo "RESULT: PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "OPENSSL_LA_ALL_PASSED"
    exit 0
fi

echo "OPENSSL_LA_HAS_FAILURES"
exit 1
