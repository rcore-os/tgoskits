#!/usr/bin/env bash
# validate.sh - HOST-ONLY validation of conf/bootstrap.yaml.
#
# Runs the real Envoy x86_64 release against the same upstreams the guest carpet
# uses (the `echod` HTTP echo backend built from backend/echod.c, plus an openssl
# s_server TLS upstream) and asserts the full documented gateway data path with
# curl + openssl s_client. This is a developer harness (not part of the guest
# image); the guest carpet is programs/run-higress.sh. It proves the bootstrap
# config + assertion logic independently of QEMU.
#
# Requires: the Envoy binary staged by prebuild.sh in $HIGRESS_CACHE
# (default ~/.cache/starry-higress-carpet), plus cc, openssl and curl.
set -u
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
cc -O2 -o "$work/echod" "$app_dir/backend/echod.c" || { echo "cc failed to build echod"; exit 1; }

openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$work/certs/server.key" \
    -out "$work/certs/server.crt" -subj "/CN=localhost" \
    -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" >/dev/null 2>&1
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$work/certs/otherca.key" \
    -out "$work/certs/otherca.crt" -subj "/CN=higress-other-ca" >/dev/null 2>&1
sed "s#/etc/higress/certs#$work/certs#g" "$conf_src" > "$work/bootstrap.yaml"
sed 's/enable_reuse_port: false/enable_reuse_port: true/' "$work/bootstrap.yaml" > "$work/bootstrap-rp.yaml"

PASS=0; TOTAL=0
ok()   { TOTAL=$((TOTAL+1)); PASS=$((PASS+1)); echo "  PASS | $1"; }
bad()  { TOTAL=$((TOTAL+1)); echo "  FAIL | $1"; }
ceq()  { if [ "$2" = "$3" ]; then ok "$1 | $2"; else bad "$1 | want[$3] got[$2]"; fi; }
cge()  { if [ "${2:-0}" -ge "$3" ] 2>/dev/null; then ok "$1 | $2>=$3"; else bad "$1 | ${2:-0} not>=$3"; fi; }
chas() { if printf '%s' "$2" | grep -q -- "$3"; then ok "$1"; else bad "$1 | missing[$3]"; fi; }
cno()  { if printf '%s' "$2" | grep -q -- "$3"; then bad "$1 | unexpected[$3]"; else ok "$1"; fi; }
c5xx() { case "$2" in 5??) ok "$1 | $2";; *) bad "$1 | want5xx got[$2]";; esac; }

B()  { curl -s "http://127.0.0.1:10000$1"; }
C()  { curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:10000$1"; }
H()  { curl -sD - -o /dev/null "http://127.0.0.1:10000$1"; }
A()  { curl -s "http://127.0.0.1:9901$1"; }
AC() { curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:9901$1"; }

pids=""
cleanup(){ for p in $pids; do kill "$p" 2>/dev/null; done; pkill -f -- "--base-id 71" 2>/dev/null; pkill -f -- "--base-id 72" 2>/dev/null; }
trap cleanup EXIT

# --- CLI surface ---
ver=$("$envoy" --version 2>&1)
chas "cli version 1.38.3" "$ver" "1.38.3"
chas "cli version keyword" "$ver" "version"
"$envoy" --mode validate -c "$work/bootstrap.yaml" >/dev/null 2>&1; ceq "cli validate good rc0" "$?" "0"
printf 'nonsense: [\n' > "$work/bad.yaml"
"$envoy" --mode validate -c "$work/bad.yaml" >/dev/null 2>&1
if [ "$?" -ne 0 ]; then ok "cli validate bad rc!=0"; else bad "cli validate bad rc!=0"; fi
chas "cli help concurrency" "$("$envoy" --help 2>&1)" "concurrency"

# --- upstreams ---
"$work/echod" 8081 backend_a ok >/dev/null 2>&1 & pids="$pids $!"
"$work/echod" 8082 backend_b ok >/dev/null 2>&1 & pids="$pids $!"
"$work/echod" 8083 backend_c ok >/dev/null 2>&1 & pids="$pids $!"
"$work/echod" 8084 backend_fail fail503 >/dev/null 2>&1 & pids="$pids $!"
"$work/echod" 8085 backend_slow slow >/dev/null 2>&1 & pids="$pids $!"
openssl s_server -accept 8443 -cert "$work/certs/server.crt" -key "$work/certs/server.key" -www -quiet >/dev/null 2>&1 & pids="$pids $!"
sleep 1

"$envoy" -c "$work/bootstrap.yaml" --concurrency 1 --disable-hot-restart --base-id 71 >"$work/envoy.log" 2>&1 &
for _ in $(seq 1 40); do [ "$(AC /ready)" = "200" ] && break; sleep 0.5; done

# --- admin ---
ceq  "admin /ready" "$(A /ready)" "LIVE"
chas "admin /stats server.state" "$(A /stats)" "server.state"
chas "admin /server_info LIVE" "$(A /server_info)" "LIVE"
chas "admin /clusters backend_a" "$(A /clusters)" "backend_a"
chas "admin /listeners http" "$(A /listeners)" "0.0.0.0:10000"
chas "admin /listeners https" "$(A /listeners)" "0.0.0.0:10443"
sf=$(A '/stats?filter=server.state')
chas "admin /stats?filter includes" "$sf" "server.state"
cno  "admin /stats?filter excludes" "$sf" "backend_a"
chas "admin /stats prometheus" "$(A '/stats?format=prometheus')" "envoy_server_"
chas "admin /config_dump node" "$(A /config_dump)" "higress-standalone"
chas "admin /certs localhost" "$(A /certs)" "localhost"
ceq  "admin unknown 404" "$(AC /no_such_admin_endpoint)" "404"

# --- routing / match ---
chas "route / -> backend_a" "$(B /)" "BACKEND=backend_a"
chas "match exact /exact" "$(H /exact)" "x-route: exact"
cno  "match exact non-match" "$(H /exactZZ)" "x-route: exact"
chas "match regex /img/a.png" "$(H /img/a.png)" "x-route: regex"
cno  "match regex non-.png" "$(H /img/a.txt)" "x-route: regex"
chas "match query ver=v2" "$(H '/qp?ver=v2')" "x-route: qp-v2"
chas "match query default" "$(H '/qp?ver=v1')" "x-route: qp-default"

# --- rewrite ---
r=$(B /api/foo/bar)
chas "rewrite prefix -> /echo" "$r" "/echo/foo/bar"
cno  "rewrite strips /api" "$r" "/api"
chas "rewrite regex capture" "$(B /rgx/hello)" "orig=hello"
chas "rewrite host literal" "$(B /hostrw)" "HOST=rewritten.example"

# --- header mutation ---
chas "req header add" "$(B /api/x)" "X_HIGRESS_ADDED=starry"
chas "resp header add" "$(H /api/x)" "x-higress-gateway: envoy-standalone"
chas "resp header present /leak" "$(H /leak)" "x-backend-secret: leak"
cno  "resp header removed" "$(H /nosecret)" "x-backend-secret"

# --- redirect / direct-response ---
ceq  "redirect 301" "$(C /redir)" "301"
chas "redirect location" "$(H /redir)" "127.0.0.1:10000/"
ceq  "direct /ping 200" "$(C /ping)" "200"
chas "direct /ping body" "$(B /ping)" "PONG"
ceq  "direct /nope 404" "$(curl -sk -o /dev/null -w '%{http_code}' https://127.0.0.1:10443/nope)" "404"
chas "direct /nope body" "$(curl -sk https://127.0.0.1:10443/nope)" "NO_ROUTE"

# --- load balancing ---
rr=""; for _ in $(seq 1 12); do rr="$rr$(B /rr)"; done
chas "rr hits backend_a" "$rr" "BACKEND=backend_a"
chas "rr hits backend_b" "$rr" "BACKEND=backend_b"
ca=0; cb=0; for _ in $(seq 1 40); do w=$(B /api/x); case "$w" in *backend_a*) ca=$((ca+1));; esac; case "$w" in *backend_b*) cb=$((cb+1));; esac; done
cge "weighted a hits" "$ca" "1"; cge "weighted b hits" "$cb" "1"
if [ "$ca" -gt "$cb" ]; then ok "weighted a>b ($ca>$cb)"; else bad "weighted a=$ca !> b=$cb"; fi
chas "least_request serves" "$(B /lr)" "BACKEND=backend_"
chas "random serves" "$(B /rand)" "BACKEND=backend_"

# --- retry / ratelimit ---
ceq "retry final 503" "$(C /flaky)" "503"
retries=$(A /stats | sed -n 's/^cluster.backend_fail.upstream_rq_retry: //p')
cge "upstream_rq_retry" "${retries:-0}" "1"
c200=0; c429=0; for _ in 1 2 3 4 5; do cc=$(C /limited); [ "$cc" = 200 ] && c200=$((c200+1)); [ "$cc" = 429 ] && c429=$((c429+1)); done
cge "ratelimit 200 allowed" "$c200" "1"; cge "ratelimit 429 throttled" "$c429" "1"

# --- exceptions ---
ceq  "dead upstream 503" "$(C /dead)" "503"
ceq  "timeout 504" "$(C /slow)" "504"
c5xx "upstream TLS verify fail" "$(C /secure-verify)"

# --- TLS ---
chas "upstream TLS /secure body" "$(B /secure)" "Ciphers supported in s_server"
ceq  "upstream TLS /secure code" "$(C /secure)" "200"
dt=$(printf 'GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n' | openssl s_client -connect 127.0.0.1:10443 -quiet 2>/dev/null)
chas "downstream TLS -> backend_a" "$dt" "BACKEND=backend_a"
si=$(openssl s_client -connect 127.0.0.1:10443 -servername localhost </dev/null 2>&1)
chas "downstream TLS cert localhost" "$si" "localhost"
if printf '%s' "$si" | grep -qE 'New, TLSv1\.(2|3)'; then ok "downstream TLS TLSv1.2+"; else bad "downstream TLS proto"; fi

# --- header-match / method / header-strip / integration (downstream TLS listener) ---
chas "header-match canary on" "$(curl -sk -D - -o /dev/null -H 'x-canary: on' https://127.0.0.1:10443/)" "x-route: canary"
cno  "header-match absent -> default" "$(curl -sk -D - -o /dev/null https://127.0.0.1:10443/)" "x-route: canary"
chas "method POST" "$(curl -sk -X POST https://127.0.0.1:10443/)" "METHOD=POST"
chas "method PUT" "$(curl -sk -X PUT https://127.0.0.1:10443/)" "METHOD=PUT"
chas "req header strip" "$(curl -sk -H 'x-strip-me: yes' https://127.0.0.1:10443/)" "X_STRIP_ME=$"
chas "integration TLS-in-TLS /secure" "$(printf 'GET /secure HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n' | openssl s_client -connect 127.0.0.1:10443 -quiet 2>/dev/null)" "Ciphers supported in s_server"

pkill -f -- "--base-id 71" 2>/dev/null; sleep 1

# --- reuse_port twin ---
"$envoy" -c "$work/bootstrap-rp.yaml" --concurrency 1 --disable-hot-restart --base-id 72 >"$work/envoy-rp.log" 2>&1 &
for _ in $(seq 1 40); do [ "$(AC /ready)" = "200" ] && break; sleep 0.5; done
ceq  "reuse_port /ready" "$(A /ready)" "LIVE"
chas "reuse_port serves" "$(B /)" "BACKEND=backend_a"
pkill -f -- "--base-id 72" 2>/dev/null

echo "HIGRESS_HOST_OK=$PASS/$TOTAL"
[ "$PASS" = "$TOTAL" ] && echo "HOST VALIDATION PASSED" || { echo "HOST VALIDATION FAILED"; exit 1; }
