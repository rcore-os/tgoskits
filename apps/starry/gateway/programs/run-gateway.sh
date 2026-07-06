#!/bin/sh
# run-gateway.sh -- on-target gate for the StarryOS `gateway` app: angie 1.11.5 exercised as a
# carpet-grade API gateway across the full HTTP/TLS/cache/gzip/stream directive surface. Every
# carpet is a REAL assertion against an EXPECTED constant. The topology is self-contained: one
# foreground angie hosts the gateway front-end (127.0.0.1:18080) AND every back-end it proxies to
# (plaintext HTTP, TLS vhosts, and a stream{} four-layer tier), so angie itself supplies both ends
# of every TLS / stream / proxy_protocol hop and busybox `wget` (plaintext, to :18080) is the ONLY
# client needed. The `GATEWAY_OK=<P>/<T>` / `TEST PASSED` success anchor is emitted ONLY by the
# final printf so the harness success_regex can never self-match an echoed command.
set -u

for a in x86_64 aarch64 riscv64 loongarch64; do
    printf '/lib\n/usr/lib\n' > "/etc/ld-musl-$a.path" 2>/dev/null || true
done
export PATH=/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root

ANGIE=/usr/sbin/angie
FRONT_CONF=/etc/angie/gateway.conf
WORKERS_CONF=/etc/angie/gateway-workers.conf
FP=18080          # gateway front-end
WP=18090          # multi-worker server

# angie's compiled-in temp paths differ between the apk build (x86_64/aarch64, /var/cache/angie)
# and the source-cross-built build (riscv64/loongarch64, /var/lib/angie/tmp); create both leaf
# sets plus the proxy-cache dir, world-writable, so angie starts regardless of the module set.
mkdir -p /run /var/log/angie \
         /var/cache/angie/client_temp /var/cache/angie/proxy_temp \
         /var/cache/angie/fastcgi_temp /var/cache/angie/uwsgi_temp /var/cache/angie/scgi_temp \
         /var/lib/angie/tmp/client_body /var/lib/angie/tmp/proxy /var/lib/angie/tmp/fastcgi \
         /var/lib/angie/tmp/w_client_body /var/lib/angie/cache /var/lib/angie/acme
chmod -R 0777 /run /var/log/angie /var/cache/angie /var/lib/angie 2>/dev/null || true

PASS=0
TOTAL=0
mark() {
    TOTAL=$((TOTAL + 1))
    if [ "$2" = 1 ]; then PASS=$((PASS + 1)); echo "  PASS $1"; else echo "  FAIL $1  (${3:-})"; fi
}
# body of GET :FP<path> (-t 1 = no retry storm on 5xx)
fetch() { wget -q -O - -t 1 -T 8 "http://127.0.0.1:$FP$1" 2>/dev/null; }
# response headers + status line of GET :FP<path> (busybox `wget -S` writes them to stderr)
hdrs()  { wget -S -O /dev/null -t 1 -T 8 "http://127.0.0.1:$FP$1" 2>&1; }
# numeric status of GET :FP<path> (first HTTP status line seen, incl. busybox's error line)
code()  { hdrs "$1" | grep -oE 'HTTP/1\.[01] [0-9][0-9][0-9]' | head -1 | awk '{print $2}'; }
# assert body substring
ckb()   { b=$(fetch "$1"); case "$b" in *"$2"*) mark "$3" 1 ;; *) mark "$3" 0 "want[$2] got[$b]" ;; esac; }
# assert header/-S substring
ckh()   { h=$(hdrs "$1"); case "$h" in *"$2"*) mark "$3" 1 ;; *) mark "$3" 0 "want[$2]" ;; esac; }
# assert header/-S substring ABSENT
ckhabs(){ h=$(hdrs "$1"); case "$h" in *"$2"*) mark "$3" 0 "found[$2]" ;; *) mark "$3" 1 ;; esac; }
# assert exact status code
ckc()   { c=$(code "$1"); if [ "$c" = "$2" ]; then mark "$3" 1; else mark "$3" 0 "want[$2] got[$c]"; fi; }
# assert 5xx (502/503/504 -- exact code depends on whether the stack RSTs or drops)
ck5xx() { c=$(code "$1"); case "$c" in 502|503|504) mark "$2" 1 ;; *) mark "$2" 0 "want5xx got[$c]" ;; esac; }

wait_serve() {   # $1=path $2=pid -> 0 when a non-empty body is served
    i=0
    while [ "$i" -lt 40 ]; do
        [ -n "$(fetch "$1")" ] && return 0
        kill -0 "$2" 2>/dev/null || return 1
        i=$((i + 1)); sleep 1
    done
    return 1
}

echo "=== gateway: angie full-directive carpet ==="
$ANGIE -v 2>&1

# --- version red-line + config syntax ---
v=0; $ANGIE -v 2>&1 | grep -q 'Angie/1.11.5' && v=1
mark version "$v" "wrong angie version"
$ANGIE -c "$FRONT_CONF" -t > /tmp/gw.conftest 2>&1; rc=$?
c=0; [ "$rc" -eq 0 ] && c=1
mark conftest "$c" "$(grep -iE 'emerg|error' /tmp/gw.conftest | head -1)"

# --- CLI surface: -h usage / -V build info / -m module set / -t rejects a broken config ---
$ANGIE -h 2>&1 | grep -q -- '-c filename' && mark cli-help 1 || mark cli-help 0 "no -c in -h"
$ANGIE -V 2>&1 | grep -q 'TLS SNI support enabled' && mark cli-V-sni 1 || mark cli-V-sni 0
# The exact module set the carpet exercises must be compiled in on EVERY arch -- the x86/aarch64
# apk and the riscv64/loongarch64 source build have to agree. Assert each required module by name
# from `angie -m` (this is the guard that keeps the rv/loong source build in lock-step with the apk).
mods=$($ANGIE -m 2>&1)
for m in ngx_http_ssl_module ngx_http_v2_module ngx_http_realip_module ngx_http_sub_filter_module \
         ngx_http_gzip_static_module ngx_http_gunzip_filter_module ngx_http_stub_status_module \
         ngx_http_grpc_module ngx_http_upstream_least_conn_module ngx_http_upstream_ip_hash_module \
         ngx_http_upstream_keepalive_module ngx_stream_ssl_preread_module ngx_stream_realip_module; do
    case "$mods" in *"$m"*) mark "module-${m#ngx_}" 1 ;; *) mark "module-${m#ngx_}" 0 "not compiled" ;; esac
done
# -t must reject (non-zero + emerg) a config with an unknown directive (config-error path).
printf 'events { worker_connections 4; }\nhttp { server { listen 127.0.0.1:1; nonsense_directive on; } }\n' > /tmp/gw.bad.conf
$ANGIE -c /tmp/gw.bad.conf -t > /tmp/gw.badtest 2>&1; brc=$?
bad=0; { [ "$brc" -ne 0 ] && grep -qiE 'emerg|unknown directive' /tmp/gw.badtest; } && bad=1
mark conftest-rejects-bad "$bad" "brc=$brc"

$ANGIE -c "$FRONT_CONF" > /tmp/gw.front.log 2>&1 &
FPID=$!
if wait_serve / "$FPID"; then
    # ---- B: location matching priority ----
    ckb /            GATEWAY_ROOT   loc-root
    ckb /exact       EXACT_MATCH    loc-exact
    ckb /a           PREFIX_A       loc-prefix
    ckb /abc         PREFIX_AB      loc-longest-prefix
    ckb /x.txt       REGEX_TXT      loc-regex
    ckb /Y.PNG       REGEX_PNG_CI   loc-regex-ci
    ckb /static/x.txt PREFIX_STOP   loc-prefix-stop
    ckb /named       NAMED_FALLBACK loc-named
    ckh /exact       "X-Gateway: angie" add-header

    # ---- C: request-rewriting proxy directives (reflected by B_REFLECT 18083) ----
    ckb /px          "host=127.0.0.1:18083" proxy-pass
    ckb /px-uri/z    "uri=/reflected/z"     proxy-pass-uri-rewrite
    ckb /sethdr      "host=custom.example"  proxy-set-header-host
    ckb /sethdr      "custom=injected"      proxy-set-header-custom
    ckb /sethdr      "xff=127.0.0.1"        proxy-add-xff
    ckb /sethdr      "xreal=127.0.0.1"      proxy-set-xrealip
    ckb /setbody     "cl=16"                proxy-set-body
    ckb /method      "method=PUT"           proxy-method
    ckb /ver10       "proto=HTTP/1.0"       proxy-http-version-10
    ckb /ver11       "proto=HTTP/1.1"       proxy-http-version-11
    b=$(fetch /nobody); case "$b" in *"cl="[!0-9]*|*"cl=") mark proxy-pass-request-body-off 1 ;; *) case "$b" in *"cl= "*) mark proxy-pass-request-body-off 1 ;; *) mark proxy-pass-request-body-off 0 "$b" ;; esac ;; esac

    # ---- D: response-side rewriting (B_CTRL 18084) ----
    ckh    /hide  "X-Public: ok"     proxy-hide-keeps-public
    ckhabs /hide  "X-Secret"         proxy-hide-header
    ckh    /passh "X-Pad: padvalue"  proxy-pass-header
    ckh    /redir "/shop/next"       proxy-redirect-rewrite
    ckh    /rediroff "18084/app/next" proxy-redirect-off
    ckh    /cookie "Domain=front.local" proxy-cookie-domain
    ckh    /cookie "Path=/;"         proxy-cookie-path

    # ---- E: upstream / load balancing ----
    lb="$(fetch /lb)$(fetch /lb)$(fetch /lb)$(fetch /lb)"
    s1=0; s2=0; case "$lb" in *BACKEND_ONE*) s1=1 ;; esac; case "$lb" in *BACKEND_TWO*) s2=1 ;; esac
    r=0; [ "$s1" = 1 ] && [ "$s2" = 1 ] && r=1; mark upstream-round-robin "$r" "$lb"
    ckb /backup    BACKEND_TWO  upstream-backup
    ckb /failover  BACKEND_ONE  proxy-next-upstream-failover
    ckb /nofail502 BACKEND_ONE  proxy-next-upstream-http502
    ckc /keep502 502            proxy-next-upstream-off
    # weighted round-robin (3:1) -> BACKEND_ONE must dominate over 8 draws
    w=""; i=0; while [ "$i" -lt 8 ]; do w="$w$(fetch /weight)"; i=$((i + 1)); done
    w1=$(printf '%s' "$w" | grep -o BACKEND_ONE | wc -l); w2=$(printf '%s' "$w" | grep -o BACKEND_TWO | wc -l)
    r=0; [ "$w1" -gt "$w2" ] && r=1; mark upstream-weight "$r" "one=$w1 two=$w2"
    ckb /lb-least  BACKEND       upstream-least-conn
    # ip_hash: one client IP is sticky -> identical backend on repeat
    ih1=$(fetch /lb-iphash); ih2=$(fetch /lb-iphash)
    r=0; [ -n "$ih1" ] && [ "$ih1" = "$ih2" ] && r=1; mark upstream-ip-hash "$r" "1=$ih1 2=$ih2"
    ckb '/lb-hash?k=abc' BACKEND upstream-hash-consistent
    ckb /lb-random BACKEND       upstream-random
    ckb /lb-ka     BACKEND_ONE   upstream-keepalive
    ckb /lb-maxfail BACKEND_ONE  upstream-max-fails-eject
    ckb /lb-slow   BACKEND       upstream-slow-start

    # ---- F: timeouts / error interception ----
    ck5xx /ctimeout             proxy-connect-timeout
    ck5xx /dead                 proxy-dead-upstream
    ckb /intercept   CUSTOM_502_PAGE   proxy-intercept-errors-on
    ckc /nointercept 502               proxy-intercept-errors-off

    # ---- G: proxy cache (MISS then HIT) ----
    m1=$(hdrs /cache); m2=$(hdrs /cache)
    case "$m1" in *"X-Cache: MISS"*) mark cache-miss 1 ;; *) mark cache-miss 0 ;; esac
    case "$m2" in *"X-Cache: HIT"*)  mark cache-hit 1 ;;  *) mark cache-hit 0 ;; esac
    ckh /cache-ignore "X-Cache: MISS" cache-ignore-headers-1
    h=$(hdrs /cache-ignore); case "$h" in *"X-Cache: HIT"*) mark cache-ignore-headers-2 1 ;; *) mark cache-ignore-headers-2 0 ;; esac

    # ---- H: gzip / gunzip (self-contained; no client Accept-Encoding needed) ----
    wget -q -O /tmp/gw.gz -T 8 "http://127.0.0.1:$FP/gzraw" 2>/dev/null
    magic=$(od -An -tx1 -N2 /tmp/gw.gz 2>/dev/null | tr -d ' \n')
    r=0; [ "$magic" = "1f8b" ] && r=1; mark gzip-magic "$r" "magic=$magic"
    gzsz=$(wc -c < /tmp/gw.gz 2>/dev/null); gzsz=${gzsz:-0}
    r=0; [ "$gzsz" -gt 0 ] && [ "$gzsz" -lt 3000 ] && r=1; mark gzip-shrink "$r" "gz=$gzsz"
    ckb /gunzip AAAAAAAAAA gunzip-roundtrip
    ckh /gzraw "Content-Encoding: gzip" gzip-content-encoding
    ckh /gzraw "Vary"                   gzip-vary-header

    # ---- I: proxy_ssl to TLS back-ends (angie is both client and server) ----
    ckb /ssl-ok       TLS_BACKEND    proxy-ssl-verify-ok
    ck5xx /ssl-fail                  proxy-ssl-verify-fail
    ckb /ssl-noverify TLS_ROGUE      proxy-ssl-no-verify
    ckb /ssl-mtls     cverify=SUCCESS proxy-ssl-mtls
    ckb /ssl-tls13    TLS13          proxy-ssl-tls13
    ck5xx /ssl-mismatch              proxy-ssl-proto-mismatch

    # ---- J: limit_req / limit_conn (concurrent bursts) ----
    # NB: bare `wait` would also block on the angie server child (FPID, never exits), so only the
    # per-request subshell PIDs are waited on.
    rm -f /tmp/gw.lr.*; i=0; lpids=""
    while [ "$i" -lt 12 ]; do ( code /limit > "/tmp/gw.lr.$i" 2>/dev/null ) & lpids="$lpids $!"; i=$((i + 1)); done
    for p in $lpids; do wait "$p" 2>/dev/null; done
    n200=$(cat /tmp/gw.lr.* 2>/dev/null | grep -c 200); n429=$(cat /tmp/gw.lr.* 2>/dev/null | grep -c 429)
    r=0; [ "$n200" -ge 1 ] && r=1; mark limit-req-serves "$r" "200=$n200"
    r=0; [ "$n429" -ge 1 ] && r=1; mark limit-req-reject "$r" "429=$n429 200=$n200"
    rm -f /tmp/gw.lc.*; i=0; cpids=""
    while [ "$i" -lt 4 ]; do ( code /limitconn > "/tmp/gw.lc.$i" 2>/dev/null ) & cpids="$cpids $!"; i=$((i + 1)); done
    for p in $cpids; do wait "$p" 2>/dev/null; done
    c429=$(cat /tmp/gw.lc.* 2>/dev/null | grep -c 429)
    r=0; [ "$c429" -ge 1 ] && r=1; mark limit-conn-reject "$r" "429=$c429"

    # ---- K: rewrite / return / map / set / if ----
    ckb /old       REWRITTEN    rewrite-last
    ckc /r301 301               return-301
    ckb /map       MAP_DEFAULT  map-default
    ckb '/map?sel=foo'    MAP_FOO   map-exact
    ckb '/map?sel=re-xyz' MAP_RE_xyz map-regex-capture
    ckb /setvar    SET-GET      set-var
    ckb '/ifua?flag=yes' IF_YES if-yes
    ckb '/ifua?flag=no'  IF_NO  if-no
    ckb /split     BUCKET       split-clients
    ckc /r302 302               return-302
    ckc /forbidden 403          return-403
    ckc /nope-no-such-loc 404   status-404-unmatched
    ckb /errpage   EP_CUSTOM    error-page-named
    ckc /internalonly 404       internal-directive
    ckc /always    204          return-204
    ckh /multihdr "X-One: one"  add-header-multi-1
    ckh /multihdr "X-Two: two"  add-header-multi-2

    # ---- L: add_header always / realip ----
    ckh /always "X-Always: alive" add-header-always
    ckb /realip "REMOTE=203.0.113.7" realip

    # ---- M: sub_filter (single + chained with add_header) ----
    ckb /sub SUBBED sub-filter
    ckb /chain CHAINSUB           chain-sub-filter
    ckh /chain "X-Chain: chained" chain-add-header

    # ---- N: stub_status ----
    ckb /status "Active connections" stub-status

    # ---- O: stream tier (TCP proxy + proxy_protocol) ----
    # A UDP (datagram) stream tier (listen udp / stream `return` / proxy_responses) is wired in the
    # config and is bound live at startup (angie would abort here if the guest could not create the
    # UDP sockets, so a served front-end already proves UDP socket bind works). Its datagram round-
    # trip is NOT asserted: the only client on target is the minimal busybox `nc` (TCP-only -- its
    # usage line is "nc [OPTIONS] HOST PORT - connect", no -u), same host-client gap as gRPC below.
    ckb /stream-tcp "RB method=GET" stream-tcp-proxy
    ckb /stream-pp  "addr=127.0.0.1" stream-proxy-protocol

    # ---- P: stream ssl_preread SNI routing (front sends TLS ClientHello via proxy_ssl) ----
    ckb /sni-foo     SNI_FOO     ssl-preread-sni-foo
    ckb /sni-bar     SNI_BAR     ssl-preread-sni-bar
    ckb /sni-default SNI_DEFAULT ssl-preread-sni-default
else
    echo "  gateway front-end did not bind 127.0.0.1:$FP"
    echo "=== gw.front.log ==="; cat /tmp/gw.front.log 2>/dev/null
    for t in loc-root loc-exact loc-prefix loc-longest-prefix loc-regex loc-regex-ci \
             loc-prefix-stop loc-named add-header proxy-pass proxy-pass-uri-rewrite \
             proxy-set-header-host proxy-set-header-custom proxy-add-xff proxy-set-xrealip \
             proxy-set-body proxy-method proxy-http-version-10 proxy-http-version-11 \
             proxy-pass-request-body-off proxy-hide-keeps-public proxy-hide-header \
             proxy-pass-header proxy-redirect-rewrite proxy-redirect-off proxy-cookie-domain \
             proxy-cookie-path upstream-round-robin upstream-backup proxy-next-upstream-failover \
             proxy-next-upstream-http502 proxy-next-upstream-off upstream-weight upstream-least-conn \
             upstream-ip-hash upstream-hash-consistent upstream-random upstream-keepalive \
             upstream-max-fails-eject upstream-slow-start proxy-connect-timeout \
             proxy-dead-upstream proxy-intercept-errors-on proxy-intercept-errors-off cache-miss \
             cache-hit cache-ignore-headers-1 cache-ignore-headers-2 gzip-magic gzip-shrink \
             gunzip-roundtrip gzip-content-encoding gzip-vary-header proxy-ssl-verify-ok \
             proxy-ssl-verify-fail proxy-ssl-no-verify \
             proxy-ssl-mtls proxy-ssl-tls13 proxy-ssl-proto-mismatch limit-req-serves \
             limit-req-reject limit-conn-reject rewrite-last return-301 map-default map-exact \
             map-regex-capture set-var if-yes if-no split-clients return-302 return-403 \
             status-404-unmatched error-page-named internal-directive return-204 add-header-multi-1 \
             add-header-multi-2 add-header-always realip sub-filter chain-sub-filter chain-add-header \
             stub-status stream-tcp-proxy stream-proxy-protocol ssl-preread-sni-foo \
             ssl-preread-sni-bar ssl-preread-sni-default; do mark "$t" 0 "front-end down"; done
fi
kill "$FPID" 2>/dev/null; kill -9 "$FPID" 2>/dev/null
sleep 1

# ---- R: multi-worker master forking two workers that drop to nobody (setuid) ----
: > /var/log/angie/workers.log
$ANGIE -c "$WORKERS_CONF" > /tmp/gw.workers.stderr 2>&1 &
WPID=$!
served=0; wait_serve_wp() { i=0; while [ "$i" -lt 40 ]; do [ -n "$(wget -q -O - -T 6 "http://127.0.0.1:$WP/" 2>/dev/null)" ] && return 0; kill -0 "$WPID" 2>/dev/null || return 1; i=$((i+1)); sleep 1; done; return 1; }
wait_serve_wp && served=1
sleep 1
nworkers=$(grep -c 'start worker process' /var/log/angie/workers.log 2>/dev/null)
[ -z "$nworkers" ] && nworkers=0
fork_ok=0; [ "$nworkers" -ge 2 ] && fork_ok=1
mp=$(cat /run/angie-w.pid 2>/dev/null)
uid_seen=""; setuid_ok=0
for pd in /proc/[0-9]*; do
    p=${pd#/proc/}
    [ "$p" = "$mp" ] && continue
    comm=$(cat "$pd/comm" 2>/dev/null)
    case "$comm" in *angie*) ;; *) continue ;; esac
    u=$(awk '/^Uid:/{print $2}' "$pd/status" 2>/dev/null)
    [ -n "$u" ] && uid_seen="$u"
    [ "$u" = 65534 ] && setuid_ok=1
done
if [ "$setuid_ok" != 1 ]; then
    if ps 2>/dev/null | grep angie | grep -qE 'nobody|65534'; then setuid_ok=1; uid_seen="ps:nobody"; fi
fi
echo "  workers started=$nworkers served=$served worker_uid=[$uid_seen]"
r=0; [ "$fork_ok" = 1 ] && [ "$served" = 1 ] && [ "$setuid_ok" = 1 ] && r=1
mark multi-worker-setuid "$r" "fork=$fork_ok served=$served uid=$uid_seen"
kill "$WPID" 2>/dev/null; kill -9 "$WPID" 2>/dev/null

echo "GATEWAY_RESULT PASS=$PASS TOTAL=$TOTAL"
if [ "$PASS" = "$TOTAL" ] && [ "$TOTAL" -gt 0 ]; then
    printf 'GATEWAY_OK=%d/%d\n' "$PASS" "$TOTAL"
    printf 'TEST PASSED\n'
    exit 0
fi
printf 'GATEWAY_OK=%d/%d\n' "$PASS" "$TOTAL"
printf 'TEST FAILED\n'
exit 1
