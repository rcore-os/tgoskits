#!/bin/sh
set -e

echo "[curl-test] start"

unset http_proxy
unset https_proxy
unset HTTP_PROXY
unset HTTPS_PROXY
unset ALL_PROXY
unset all_proxy

URL="http://10.0.2.2:8000/"
EXPECTED="hello from host"

echo "[curl-test] install curl"
apk update >/dev/null 2>&1
apk add curl ca-certificates >/dev/null 2>&1

echo "[curl-test] curl version"
curl --version

echo "[curl-test] testing: $URL"
OUT="$(curl --noproxy '*' -sS --connect-timeout 5 --max-time 15 "$URL")"

echo "[curl-test] response:"
echo "$OUT"

echo "$OUT" | grep "$EXPECTED"

echo "[curl-test] passed"
