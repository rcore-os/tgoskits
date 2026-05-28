#!/bin/sh
set -e

echo "[curl-test] start"

# 避免宿主环境/镜像里残留代理变量，导致测试走代理而不是直连
unset http_proxy
unset https_proxy
unset HTTP_PROXY
unset HTTPS_PROXY
unset ALL_PROXY
unset all_proxy

URL="http://10.0.2.2:8000/"
EXPECTED="hello from host"

echo "[curl-test] testing: $URL"

OUT="$(curl --noproxy '*' -sS "$URL")"

echo "[curl-test] response:"
echo "$OUT"

echo "$OUT" | grep "$EXPECTED"

echo "[curl-test] passed"

