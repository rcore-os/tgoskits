#!/bin/sh
set -eu

payload=/tmp/apk-curl-equivalence-payload.bin
payload_sum=/tmp/apk-curl-equivalence-payload.sha256
payload_size=4194304
payload_sha256=299285fc41a44cdb038b9fdaf494c76ca9d0c866672b2b266c1a0c17dda60a05

fail() {
    echo "APK_CURL_EQUIVALENCE_TEST_FAILED: $1"
    exit 1
}

case "$(uname -m)" in
    x86_64) port=18380 ;;
    aarch64) port=18381 ;;
    riscv64) port=18382 ;;
    loongarch64) port=18383 ;;
    *) fail "unsupported arch $(uname -m)" ;;
esac

url="http://10.0.2.2:$port/payload.bin"

echo "APK_CURL_EQUIVALENCE_DOWNLOAD_BEGIN"
rm -f "$payload" "$payload_sum"
curl --connect-timeout 10 --max-time 180 -fsSL "$url" -o "$payload" ||
    fail "curl download failed"

actual_size="$(wc -c < "$payload" | tr -d ' ')"
[ "$actual_size" = "$payload_size" ] ||
    fail "size mismatch expected=$payload_size actual=$actual_size"

printf '%s  %s\n' "$payload_sha256" "$payload" > "$payload_sum"
sha256sum -c "$payload_sum" ||
    fail "sha256 mismatch"

echo "APK_CURL_EQUIVALENCE_TEST_PASSED"
