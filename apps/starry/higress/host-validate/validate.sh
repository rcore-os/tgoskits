#!/usr/bin/env bash
# validate.sh - HOST-ONLY validation of conf/bootstrap.yaml.
#
# Runs the real Envoy x86_64 release against python echo backends and asserts the
# full gateway data path with curl. This is a developer harness (python + curl,
# not part of the guest image); the guest carpet is programs/run-higress.sh. It
# proves the bootstrap config + assertion logic independently of QEMU.
#
# Requires: the Envoy binary staged by prebuild.sh in
# $HIGRESS_CACHE (default ~/.cache/starry-higress-carpet), python3, curl, openssl.
set -u
# Loopback only; drop any global proxy that would hijack curl to 127.0.0.1.
unset http_proxy https_proxy all_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY
export no_proxy='*' NO_PROXY='*'

here="$(cd "$(dirname "$0")" && pwd)"
app_dir="$(cd "$here/.." && pwd)"
conf_src="$app_dir/conf/bootstrap.yaml"
cache="${HIGRESS_CACHE:-${HOME:-/root}/.cache/starry-higress-carpet}"
envoy="$cache/envoy-1.38.3-linux-x86_64"
work="$here/.work"
rm -rf "$work"; mkdir -p "$work/certs"

[ -x "$envoy" ] || { echo "missing $envoy - run prebuild.sh (STARRY_ARCH=x86_64) first"; exit 1; }

# Self-contained certs + a config copy that points at them.
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$work/certs/server.key" -out "$work/certs/server.crt" \
    -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" >/dev/null 2>&1
sed "s#/etc/higress/certs#$work/certs#g" "$conf_src" > "$work/bootstrap.yaml"
sed 's/enable_reuse_port: false/enable_reuse_port: true/' "$work/bootstrap.yaml" > "$work/bootstrap-reuseport.yaml"

PASS=0; TOTAL=0
check()    { TOTAL=$((TOTAL+1)); if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "  PASS | $1 | $2"; else echo "  FAIL | $1 | expected[$3] got[$2]"; fi; }
check_ge() { TOTAL=$((TOTAL+1)); if [ "${2:-0}" -ge "$3" ] 2>/dev/null; then PASS=$((PASS+1)); echo "  PASS | $1 | $2>=$3"; else echo "  FAIL | $1 | ${2:-0} not >= $3"; fi; }
check_has(){ TOTAL=$((TOTAL+1)); if printf '%s' "$2" | grep -q -- "$3"; then PASS=$((PASS+1)); echo "  PASS | $1"; else echo "  FAIL | $1 | missing[$3]"; fi; }

pids=""
cleanup() { for p in $pids; do kill "$p" 2>/dev/null; done; pkill -f -- "--base-id 1" 2>/dev/null; pkill -f -- "--base-id 2" 2>/dev/null; }
trap cleanup EXIT

"$envoy" --mode validate -c "$work/bootstrap.yaml" >/dev/null 2>&1
check "envoy --mode validate" "$?" "0"

python3 "$here/echo_backend.py" backend_a 8081 200 & pids="$pids $!"
python3 "$here/echo_backend.py" backend_b 8082 200 & pids="$pids $!"
python3 "$here/echo_backend.py" backend_c 8083 200 & pids="$pids $!"
python3 "$here/echo_backend.py" backend_fail 8084 503 & pids="$pids $!"
python3 "$here/tls_backend.py" 8443 "$work/certs/server.crt" "$work/certs/server.key" & pids="$pids $!"
sleep 1

"$envoy" -c "$work/bootstrap.yaml" --concurrency 1 --disable-hot-restart --base-id 1 >"$work/envoy.log" 2>&1 &
for _ in $(seq 1 40); do [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:9901/ready)" = "200" ] && break; sleep 0.5; done
check "admin /ready" "$(curl -s http://127.0.0.1:9901/ready)" "LIVE"

check_has "route / -> backend_a" "$(curl -s http://127.0.0.1:10000/)" "BACKEND=backend_a"

ca=0; cb=0
for _ in $(seq 1 18); do
  r=$(curl -s http://127.0.0.1:10000/api/x)
  case "$r" in *backend_a*) ca=$((ca+1));; esac
  case "$r" in *backend_b*) cb=$((cb+1));; esac
done
echo "  (weighted LB tally: backend_a=$ca backend_b=$cb)"
check_ge "weighted LB backend_a" "$ca" "1"
check_ge "weighted LB backend_b" "$cb" "1"
TOTAL=$((TOTAL+1)); if [ "$ca" -gt "$cb" ]; then PASS=$((PASS+1)); echo "  PASS | weighted LB a>b"; else echo "  FAIL | weighted LB a=$ca not > b=$cb"; fi

r=$(curl -s http://127.0.0.1:10000/api/foo/bar)
check_has "path rewrite -> /cgi-bin/echo" "$r" "/cgi-bin/echo"
check_has "request header x-higress-added" "$r" "X_HIGRESS_ADDED=starry"
check_has "response header x-higress-gateway" "$(curl -sD - -o /dev/null http://127.0.0.1:10000/api/x)" "x-higress-gateway: envoy-standalone"

check "retry final status 503" "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:10000/flaky)" "503"
retries=$(curl -s http://127.0.0.1:9901/stats | sed -n 's/^cluster.backend_fail.upstream_rq_retry: //p')
check_ge "upstream_rq_retry" "${retries:-0}" "1"

c200=0; c429=0
for _ in 1 2 3 4 5; do
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:10000/limited)
  [ "$code" = "200" ] && c200=$((c200+1))
  [ "$code" = "429" ] && c429=$((c429+1))
done
echo "  (ratelimit tally: 200=$c200 429=$c429)"
check_ge "local_ratelimit 200" "$c200" "1"
check_ge "local_ratelimit 429" "$c429" "1"

check_has "downstream TLS -> backend_a" "$(curl -sk https://127.0.0.1:10443/)" "BACKEND=backend_a"
check_has "upstream TLS -> backend_tls" "$(curl -s http://127.0.0.1:10000/secure)" "UPSTREAM_TLS=ok"
check_has "admin /stats server.state" "$(curl -s http://127.0.0.1:9901/stats)" "server.state"

pkill -f -- "--base-id 1" 2>/dev/null; sleep 1

"$envoy" --mode validate -c "$work/bootstrap-reuseport.yaml" >/dev/null 2>&1
check "reuseport config validate" "$?" "0"
"$envoy" -c "$work/bootstrap-reuseport.yaml" --concurrency 1 --disable-hot-restart --base-id 2 >"$work/envoy-rp.log" 2>&1 &
for _ in $(seq 1 40); do [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:9901/ready)" = "200" ] && break; sleep 0.5; done
check "admin /ready (reuse_port:true)" "$(curl -s http://127.0.0.1:9901/ready)" "LIVE"
check_has "reuse_port listener serves traffic" "$(curl -s http://127.0.0.1:10000/)" "BACKEND=backend_a"
pkill -f -- "--base-id 2" 2>/dev/null

echo "HIGRESS_HOST_OK=$PASS/$TOTAL"
[ "$PASS" = "$TOTAL" ] && echo "HOST VALIDATION PASSED" || { echo "HOST VALIDATION FAILED"; exit 1; }
