#!/bin/sh
# run-dnsmasq.sh - on-target gate for the StarryOS dnsmasq DNS/DHCP/TFTP carpet.
#
# Staged into the rootfs by prebuild.sh and invoked as the ENTIRE shell_init_cmd
# (`sh /usr/bin/run-dnsmasq.sh`). The gate lives in a staged script, not inline in the
# toml, so the harness never echoes a literal TEST PASSED back over the serial console and
# self-matches success_regex: TEST PASSED is printed ONLY by this script, ONLY when every
# assertion passed AND the count equals the pinned EXPECTED total (a skipped or silently
# dropped assertion changes TOTAL and fails the gate).
#
# dnsmasq is a single musl-dynamic binary (/usr/sbin/dnsmasq) that needs only the base
# rootfs libc; the TFTP client (tftp-hpa /usr/bin/tftp) is the same. The DNS clients
# (busybox nslookup, with -type= for every record class) are base busybox applets, so those
# two staged binaries are all the carpet needs. Provisioning is on-target `apk add` against
# the branch that matches the running rootfs (apk resolves the CURRENT version - no pinned,
# drifting URL); when the binaries are already present (staged overlay, or a host chroot
# pre-flight) the apk step is skipped and the very same carpet runs unchanged.
#
# What is exercised, all single-node over IPv4 loopback:
#   A  binary self-certification: identity + compile options + the full option surface +
#      the real config parser accepting a good config and rejecting a broken one.
#   B  an authoritative instance serving every DNS record class this build supports
#      (A / hosts / wildcard / TXT / CNAME / MX / SRV / PTR / host-record A+AAAA+PTR), each
#      queried for real over 127.0.0.1:53 and byte-checked.
#   C  a forwarder + upstream pair proving --server zone + default forwarding, custom --port,
#      never-forward --local zones, real answer caching AND the live cache/query statistics
#      dnsmasq dumps on SIGUSR1.
#   D  the DHCP server config surface driven through dnsmasq's real config parser (ranges,
#      static hosts, options, hostsfile/optsfile, vendor/mac/tag matching) with the malformed
#      specs rejected.
#   E  records loaded from a conf-file and a conf-dir, served for real.
#   F  the integrated TFTP server: a real client fetches a single-block and a multi-block file
#      over loopback and the bytes are verified, and a missing file is rejected.
#   G  an integration instance combining hosts, local records and forwarding the way a real
#      edge dnsmasq deployment is used.
#
# Live DHCP address assignment is intentionally NOT attempted: dnsmasq's Linux DHCP server
# opens an AF_PACKET/SOCK_RAW frame socket at init and a DHCP client (busybox udhcpc) sends
# its DHCPDISCOVER the same way, and the StarryOS packet-socket path is an ARP-only stub - so
# a real lease cannot round-trip on this kernel. The DHCP surface is therefore driven through
# the real config parser (Section D); the boundary is documented in the README.
set -u

export PATH=/usr/local/bin:/usr/bin:/usr/sbin:/bin:/sbin
export HOME=/root
WORK="${DNSMASQ_WORK:-/root/dnsmasq-carpet}"

DM="${DM_DNSMASQ:-dnsmasq}"          # resolved on PATH: /usr/sbin/dnsmasq
BB="${DM_BUSYBOX:-/bin/busybox}"     # base busybox: applets nslookup / etc.
NS="$BB nslookup"                    # nslookup -type=QUERY_TYPE HOST SERVER
TFTP="${DM_TFTP:-tftp}"              # tftp-hpa client: tftp -m octet HOST PORT -c get R L

# Distinct loopback ports. The DNS clients (busybox nslookup) can only target port 53, so
# every instance that gets queried directly binds 127.0.0.1:53 and is torn down before the
# next one starts; the upstream / tftp helpers use their own ports.
DP=53          # primary DNS port (queried by nslookup)
UPP=5353       # upstream forwarding target (custom port - also proves --port)
TDP=5354       # tftp helper's DNS port (keeps a loopback listener so tftp binds 127.0.0.1:69)

# Bring loopback up if the platform left it down; a configured 127.0.0.1 is all we need.
ip link set lo up 2>/dev/null || ifconfig lo up 2>/dev/null || true
ip addr add 127.0.0.1/8 dev lo 2>/dev/null || ifconfig lo 127.0.0.1 up 2>/dev/null || true

rm -rf "$WORK"; mkdir -p "$WORK"

# Every assertion below must be accounted for; a drift between TOTAL and EXPECTED is a failure.
EXPECTED=46

PASS=0
TOTAL=0
ok() { # ok <0|1> <label>
    TOTAL=$((TOTAL + 1))
    if [ "$1" = 1 ]; then PASS=$((PASS + 1)); echo "  OK   $2"; else echo "  FAIL $2"; fi
}

# Shared server launcher: foreground (-k) so the pid is ours to kill, logging to stderr
# (--log-facility=-), never dropping privileges (-u/-g root), never reading the default
# /etc/dnsmasq.conf (-C /dev/null) so only the passed options apply. Echoes the pid.
start_dns() { # start_dns <logfile> <args...>
    _log="$1"; shift
    "$DM" -k --log-facility=- -u root -g root -C /dev/null \
        --bind-interfaces --no-resolv "$@" > "$_log" 2>&1 &
    echo $!
}

# a_of <name> : echo the A address busybox resolves for <name> from 127.0.0.1 (empty on fail).
a_of() { $NS -type=a "$1" 127.0.0.1 2>/dev/null | awk '/^Address:/{a=$2} END{print a}'; }

# wait_dns <name> <expect-ip> : poll until the primary server answers, or give up.
wait_dns() {
    _i=0
    while [ "$_i" -lt 20 ]; do
        [ "$(a_of "$1")" = "$2" ] && return 0
        _i=$((_i + 1)); sleep 1
    done
    return 1
}

# ---------------------------------------------------------------------------------------
# Provision dnsmasq + tftp-hpa via on-target apk add, matching the rootfs Alpine branch.
# Skipped when the binaries are already resolvable (staged overlay, or a host pre-flight).
apk_branch() {
    if [ -n "${DNSMASQ_APK_BRANCH:-}" ]; then printf '%s\n' "$DNSMASQ_APK_BRANCH"; return; fi
    rel=""; [ -r /etc/alpine-release ] && rel="$(cat /etc/alpine-release 2>/dev/null)"
    maj="$(printf '%s' "$rel" | cut -d. -f1)"; min="$(printf '%s' "$rel" | cut -d. -f2)"
    if [ -n "$maj" ] && [ -n "$min" ]; then printf 'v%s.%s\n' "$maj" "$min"; else printf 'latest-stable\n'; fi
}

apk_add_from() { # apk_add_from <mirror>
    _m="$1"; _b="$(apk_branch)"
    cat > "$WORK/repositories" <<EOF
$_m/$_b/main
$_m/$_b/community
EOF
    echo "  apk: $_m/$_b"
    timeout "${DNSMASQ_APK_TIMEOUT:-180}" apk --no-progress --update-cache \
        --repositories-file "$WORK/repositories" add dnsmasq tftp-hpa > "$WORK/apk.log" 2>&1
}

have_tools() { command -v "$DM" >/dev/null 2>&1 && command -v "$TFTP" >/dev/null 2>&1; }

provision() {
    if have_tools; then
        echo "=== provision: dnsmasq + tftp already present, skipping apk ==="; return 0
    fi
    echo "=== provision: apk add dnsmasq tftp-hpa (branch-matched, current version) ==="
    for m in "${DNSMASQ_APK_MIRROR:-https://mirrors.tuna.tsinghua.edu.cn/alpine}" \
             "https://dl-cdn.alpinelinux.org/alpine"; do
        if apk_add_from "$m" && have_tools; then echo "  apk add OK"; return 0; fi
        tail -6 "$WORK/apk.log" 2>/dev/null
    done
    echo "  apk add FAILED on every mirror"; return 1
}

provision || { echo "PROVISION FAILED"; echo "DNSMASQ_OK=0/$EXPECTED"; echo "TEST FAILED"; exit 1; }

######################## SECTION A: BINARY SELF-CERTIFICATION ########################
# dnsmasq proves it loads and runs by reporting its identity, compile options and the full
# option surface, and by validating a good config AND rejecting a broken one through its real
# parser.
echo "=== A. binary self-certification (version / compile options / help / --test) ==="

VER="$("$DM" --version 2>&1 | tr -d '\r')"
if echo "$VER" | grep -q 'Dnsmasq version 2\.9'; then ok 1 "A1 dnsmasq --version ($(echo "$VER" | head -1))"; else ok 0 "A1 dnsmasq --version"; echo "$VER"; fi

# A2 compile-time options enumerate the subsystems this carpet exercises.
if echo "$VER" | grep -q 'DHCP' && echo "$VER" | grep -q 'DHCPv6' && \
   echo "$VER" | grep -q 'TFTP' && echo "$VER" | grep -q 'IPv6' && echo "$VER" | grep -q 'auth'; then
    ok 1 "A2 --version compile options (DHCP/DHCPv6/TFTP/IPv6/auth)"
else
    ok 0 "A2 --version compile options"; echo "$VER"
fi

# A3 the option surface: every long option this carpet drives is advertised by --help.
"$DM" --help 2>&1 | tr -d '\r' > "$WORK/help.out"
if grep -q -- '--listen-address' "$WORK/help.out" && grep -q -- '--address=' "$WORK/help.out" && \
   grep -q -- '--server=' "$WORK/help.out" && grep -q -- '--txt-record=' "$WORK/help.out" && \
   grep -q -- '--cname=' "$WORK/help.out" && grep -q -- '--mx-host=' "$WORK/help.out" && \
   grep -q -- '--srv-host=' "$WORK/help.out" && grep -q -- '--ptr-record=' "$WORK/help.out" && \
   grep -q -- '--host-record=' "$WORK/help.out" && grep -q -- '--dhcp-range=' "$WORK/help.out" && \
   grep -q -- '--dhcp-host=' "$WORK/help.out" && grep -q -- '--dhcp-option=' "$WORK/help.out" && \
   grep -q -- '--cache-size=' "$WORK/help.out" && grep -q -- '--conf-file=' "$WORK/help.out" && \
   grep -q -- '--port=' "$WORK/help.out" && grep -q -- '--enable-tftp' "$WORK/help.out"; then
    ok 1 "A3 --help enumerates DNS + DHCP + TFTP option surface"
else
    ok 0 "A3 --help option surface"; tail -8 "$WORK/help.out"
fi

# A4 the DHCP option registry is enumerable and carries the well-known numbers.
"$DM" --help dhcp 2>&1 | tr -d '\r' > "$WORK/helpdhcp.out"
if grep -qi 'Known DHCP options' "$WORK/helpdhcp.out" && grep -qE '^[[:space:]]*1 netmask' "$WORK/helpdhcp.out" && \
   grep -qE '^[[:space:]]*3 router' "$WORK/helpdhcp.out" && grep -qE '^[[:space:]]*6 dns-server' "$WORK/helpdhcp.out"; then
    ok 1 "A4 --help dhcp registry (netmask=1 router=3 dns-server=6)"
else
    ok 0 "A4 --help dhcp registry"; tail -8 "$WORK/helpdhcp.out"
fi

# A5 the config parser validates a good config (real parse, not a stub).
cat > "$WORK/good.conf" <<EOF
port=53
domain=example.org
cache-size=300
address=/blocked.test/0.0.0.0
txt-record=info.example.org,carpet
EOF
"$DM" --test -C "$WORK/good.conf" > "$WORK/a5.out" 2>&1
ARC=$?
if [ "$ARC" = 0 ] && grep -qi 'syntax check OK' "$WORK/a5.out"; then ok 1 "A5 --test validates good config (syntax check OK)"; else ok 0 "A5 --test good config (rc=$ARC)"; cat "$WORK/a5.out"; fi

# A6 the config parser REJECTS a broken config (unknown directive) - negative for --test.
cat > "$WORK/bad.conf" <<EOF
port=53
this-is-not-a-real-directive=nonsense
EOF
"$DM" --test -C "$WORK/bad.conf" > "$WORK/a6.out" 2>&1
ARC=$?
if [ "$ARC" != 0 ] && ! grep -qi 'syntax check OK' "$WORK/a6.out"; then ok 1 "A6 --test rejects broken config (rc=$ARC)"; else ok 0 "A6 --test broken config not rejected (rc=$ARC)"; cat "$WORK/a6.out"; fi

######################## SECTION B: DNS RECORDS (AUTHORITATIVE) ########################
# One instance serves every record class this build supports. Each is queried for real over
# 127.0.0.1:53 with the matching busybox nslookup -type=, and the answer is byte-checked.
echo "=== B. DNS records: authoritative instance on 127.0.0.1:$DP ==="

cat > "$WORK/hosts" <<EOF
10.0.0.10 alpha.lan alpha
10.0.0.11 beta.lan
10.0.0.12 shorty
EOF

BPID="$(start_dns "$WORK/b.log" -p "$DP" --listen-address=127.0.0.1 --no-hosts \
    --addn-hosts="$WORK/hosts" --domain=lan --expand-hosts \
    --address=/wild.test/10.9.9.9 \
    --txt-record=info.lan,"carpet text ok" \
    --cname=cn.lan,alpha.lan \
    --mx-host=mail.lan,alpha.lan,10 \
    --srv-host=_sip._tcp.lan,alpha.lan,5060,1,20 \
    --ptr-record=99.0.0.10.in-addr.arpa,ptrtarget.lan \
    --host-record=combo.lan,10.0.0.88,fd00::88 \
    --cache-size=256 --local-ttl=42 --log-queries --pid-file="$WORK/b.pid")"

if wait_dns alpha.lan 10.0.0.10; then
    ok 1 "B1 /etc/hosts-format A record (alpha.lan=10.0.0.10)"
    BUP=1
else
    ok 0 "B1 authoritative server up"; tail -8 "$WORK/b.log"; BUP=0
fi

if [ "$BUP" = 1 ]; then
    # B2 --expand-hosts: a bare hosts name is expanded with the --domain suffix.
    R="$(a_of shorty.lan)"; [ "$R" = 10.0.0.12 ] && ok 1 "B2 --expand-hosts (shorty.lan=10.0.0.12)" || ok 0 "B2 --expand-hosts (got:[$R])"

    # B3/B4 --address=/domain/ipaddr wildcard: any host under the domain returns the address.
    R="$(a_of foo.wild.test)"; [ "$R" = 10.9.9.9 ] && ok 1 "B3 --address wildcard (foo.wild.test=10.9.9.9)" || ok 0 "B3 --address wildcard (got:[$R])"
    R="$(a_of bar.baz.wild.test)"; [ "$R" = 10.9.9.9 ] && ok 1 "B4 --address wildcard (deep sub bar.baz.wild.test=10.9.9.9)" || ok 0 "B4 --address deep (got:[$R])"

    # B5 --txt-record
    R="$($NS -type=txt info.lan 127.0.0.1 2>/dev/null | tr -d '\r')"
    echo "$R" | grep -q 'text = "carpet text ok"' && ok 1 "B5 --txt-record TXT" || { ok 0 "B5 --txt-record"; echo "$R"; }

    # B6 --cname
    R="$($NS -type=cname cn.lan 127.0.0.1 2>/dev/null | tr -d '\r')"
    echo "$R" | grep -q 'canonical name = alpha.lan' && ok 1 "B6 --cname CNAME (cn.lan -> alpha.lan)" || { ok 0 "B6 --cname"; echo "$R"; }

    # B7 --mx-host
    R="$($NS -type=mx mail.lan 127.0.0.1 2>/dev/null | tr -d '\r')"
    echo "$R" | grep -q 'mail exchanger = 10 alpha.lan' && ok 1 "B7 --mx-host MX (pref 10 alpha.lan)" || { ok 0 "B7 --mx-host"; echo "$R"; }

    # B8 --srv-host (priority weight port target)
    R="$($NS -type=srv _sip._tcp.lan 127.0.0.1 2>/dev/null | tr -d '\r')"
    echo "$R" | grep -q 'service = 1 20 5060 alpha.lan' && ok 1 "B8 --srv-host SRV (1 20 5060 alpha.lan)" || { ok 0 "B8 --srv-host"; echo "$R"; }

    # B9 --ptr-record explicit reverse
    R="$($NS -type=ptr 99.0.0.10.in-addr.arpa 127.0.0.1 2>/dev/null | tr -d '\r')"
    echo "$R" | grep -q 'name = ptrtarget.lan' && ok 1 "B9 --ptr-record PTR (-> ptrtarget.lan)" || { ok 0 "B9 --ptr-record"; echo "$R"; }

    # B10 --host-record forward A
    R="$(a_of combo.lan)"; [ "$R" = 10.0.0.88 ] && ok 1 "B10 --host-record A (combo.lan=10.0.0.88)" || ok 0 "B10 --host-record A (got:[$R])"

    # B11 --host-record forward AAAA
    R="$($NS -type=aaaa combo.lan 127.0.0.1 2>/dev/null | awk '/^Address:/{a=$2} END{print a}')"
    [ "$R" = fd00::88 ] && ok 1 "B11 --host-record AAAA (combo.lan=fd00::88)" || ok 0 "B11 --host-record AAAA (got:[$R])"

    # B12 --host-record auto-generated reverse PTR
    R="$($NS -type=ptr 88.0.0.10.in-addr.arpa 127.0.0.1 2>/dev/null | tr -d '\r')"
    echo "$R" | grep -q 'name = combo.lan' && ok 1 "B12 --host-record auto PTR (10.0.0.88 -> combo.lan)" || { ok 0 "B12 --host-record PTR"; echo "$R"; }

    # B13 negative: an unknown name in the served zone is rejected (nslookup exits non-zero).
    $NS -type=a nonexistent.lan 127.0.0.1 >/dev/null 2>&1
    [ "$?" != 0 ] && ok 1 "B13 unknown name rejected (nslookup rc!=0)" || ok 0 "B13 unknown name not rejected"

    # B14 --pid-file carries the live pid.
    PF="$(cat "$WORK/b.pid" 2>/dev/null | tr -d '\r\n')"
    if [ -n "$PF" ] && kill -0 "$PF" 2>/dev/null; then ok 1 "B14 --pid-file live pid ($PF)"; else ok 0 "B14 --pid-file (pf:[$PF])"; fi

    # B15 --cache-size is honored (startup log reflects the configured size).
    grep -q 'cachesize 256' "$WORK/b.log" && ok 1 "B15 --cache-size honored (cachesize 256)" || { ok 0 "B15 --cache-size"; grep -o 'cachesize [0-9]*' "$WORK/b.log" | head -1; }
else
    for a in B2 B3 B4 B5 B6 B7 B8 B9 B10 B11 B12 B13 B14 B15; do ok 0 "$a (server down)"; done
fi
kill "$BPID" 2>/dev/null; kill -9 "$BPID" 2>/dev/null; sleep 1

######################## SECTION C: FORWARDING / CACHING / LOCAL ########################
# A forwarder on :53 delegates a zone to an upstream on a custom port, proving --server,
# custom --port, answer caching and never-forward local zones. Only the forwarder is queried
# directly (busybox nslookup targets :53); the upstream is reached through the forwarder.
echo "=== C. forwarding + caching + local (forwarder :$DP -> upstream :$UPP) ==="

UPID="$(start_dns "$WORK/up.log" -p "$UPP" --listen-address=127.0.0.1 --no-hosts \
    --address=/up.test/172.16.0.7 --address=/def.test/172.16.0.9 --local-ttl=3600 \
    --pid-file="$WORK/up.pid")"
FPID="$(start_dns "$WORK/fwd.log" -p "$DP" --listen-address=127.0.0.1 --no-hosts \
    --server=/up.test/127.0.0.1#$UPP --server=127.0.0.1#$UPP \
    --local=/loc.test/ --address=/loc.test/10.5.5.5 \
    --log-queries --cache-size=150 --pid-file="$WORK/fwd.pid")"
sleep 2

UPF="$(cat "$WORK/up.pid" 2>/dev/null | tr -d '\r\n')"
FWF="$(cat "$WORK/fwd.pid" 2>/dev/null | tr -d '\r\n')"
if [ -n "$UPF" ] && kill -0 "$UPF" 2>/dev/null && [ -n "$FWF" ] && kill -0 "$FWF" 2>/dev/null; then
    ok 1 "C1 upstream + forwarder both up (pids $UPF/$FWF)"; CUP=1
else
    ok 0 "C1 upstream + forwarder up"; tail -6 "$WORK/up.log"; tail -6 "$WORK/fwd.log"; CUP=0
fi

if [ "$CUP" = 1 ]; then
    # C2 --server=/domain/ip#port : the forwarder relays the zone to the custom-port upstream.
    R="$(a_of host.up.test)"; [ "$R" = 172.16.0.7 ] && ok 1 "C2 --server zone forward via :$UPP (host.up.test=172.16.0.7)" || ok 0 "C2 zone forward (got:[$R])"

    # C3 caching: repeat the query; the forwarder answers the second one from cache.
    a_of host.up.test >/dev/null 2>&1; a_of host.up.test >/dev/null 2>&1
    grep -qi 'cached host.up.test' "$WORK/fwd.log" && ok 1 "C3 forwarder caches upstream answer" || { ok 0 "C3 caching"; grep -i cached "$WORK/fwd.log" | tail -2; }

    # C4 --server default (no domain): an unrelated zone still reaches the default upstream.
    R="$(a_of node.def.test)"; [ "$R" = 172.16.0.9 ] && ok 1 "C4 --server default upstream (node.def.test=172.16.0.9)" || ok 0 "C4 default upstream (got:[$R])"

    # C5 --local=/domain/ : the local zone is answered locally, never forwarded.
    R="$(a_of thing.loc.test)"; [ "$R" = 10.5.5.5 ] && ok 1 "C5 --local never-forward zone (thing.loc.test=10.5.5.5)" || ok 0 "C5 --local zone (got:[$R])"

    # C6 SIGUSR1 makes dnsmasq dump live cache + query statistics to its log - real metrics,
    # not a log-string guess: the cache size, forwarded-query and answered-locally counters
    # all appear and the forwarded count is non-zero after the queries above.
    kill -USR1 "$FWF" 2>/dev/null; sleep 2
    ST="$(grep -E 'cache size|queries forwarded|queries answered locally' "$WORK/fwd.log" | tail -6)"
    FWD_N="$(printf '%s\n' "$ST" | sed -n 's/.*queries forwarded \([0-9]*\).*/\1/p' | tail -1)"
    if printf '%s\n' "$ST" | grep -q 'cache size 150' && \
       printf '%s\n' "$ST" | grep -q 'queries answered locally' && \
       [ -n "$FWD_N" ] && [ "$FWD_N" -ge 1 ] 2>/dev/null; then
        ok 1 "C6 SIGUSR1 cache/query stats (cache size 150, forwarded=$FWD_N)"
    else
        ok 0 "C6 SIGUSR1 stats"; printf '%s\n' "$ST"
    fi
else
    for a in C2 C3 C4 C5 C6; do ok 0 "$a (forwarder down)"; done
fi
kill "$FPID" "$UPID" 2>/dev/null; kill -9 "$FPID" "$UPID" 2>/dev/null; sleep 1

######################## SECTION D: DHCP CONFIG SURFACE ########################
# Live address assignment needs a DHCP client (busybox udhcpc) that sends its DHCPDISCOVER
# over an AF_PACKET frame socket, and dnsmasq's Linux DHCP server opens the same kind of
# socket at init; the StarryOS packet-socket path is an ARP-only stub, so a real lease cannot
# round-trip on this kernel (documented in the README). The DHCP server surface is instead
# driven through dnsmasq's real config parser: every server-side spec is accepted and the
# malformed ones rejected - the exact parse path dnsmasq runs before it ever binds a socket.
echo "=== D. DHCP config surface (real parser via --test) ==="

dtest() { # dtest <label> <expect-rc> <args...>
    _lbl="$1"; _exp="$2"; shift 2
    "$DM" --test -C /dev/null "$@" > "$WORK/d.out" 2>&1
    _rc=$?
    if [ "$_rc" = "$_exp" ]; then ok 1 "$_lbl (rc=$_rc)"; else ok 0 "$_lbl (rc=$_rc want $_exp)"; cat "$WORK/d.out"; fi
}

dtest "D1 --dhcp-range with netmask + lease time" 0 \
    --dhcp-range=192.168.55.50,192.168.55.150,255.255.255.0,12h
dtest "D2 --dhcp-host static MAC binding" 0 \
    --dhcp-range=192.168.55.50,192.168.55.150,12h --dhcp-host=11:22:33:44:55:66,192.168.55.77,staticbox
dtest "D3 --dhcp-option router + dns-server" 0 \
    --dhcp-range=192.168.55.50,192.168.55.150,12h --dhcp-option=3,192.168.55.1 --dhcp-option=6,192.168.55.1
dtest "D4 --dhcp-boot + authoritative + read-ethers + lease-max" 0 \
    --dhcp-range=192.168.55.50,192.168.55.150,12h --dhcp-boot=pxelinux.0 --dhcp-authoritative --read-ethers --dhcp-lease-max=500
printf '11:22:33:44:55:66,192.168.55.77,staticbox\n' > "$WORK/dhosts"
printf 'option:router,192.168.55.1\n' > "$WORK/dopts"
dtest "D5 --dhcp-hostsfile + --dhcp-optsfile from files" 0 \
    --dhcp-range=192.168.55.50,192.168.55.150,12h --dhcp-hostsfile="$WORK/dhosts" --dhcp-optsfile="$WORK/dopts"
dtest "D6 malformed --dhcp-range rejected" 1 \
    --dhcp-range=NOTanIPatall
# D7 tag-scoped options: a network-id tag on the range, an option forced onto tagged clients,
# and a vendor-class match that sets the tag - the classifying surface a real deployment uses.
dtest "D7 --dhcp-range set:tag + --dhcp-option-force tag + --dhcp-vendorclass" 0 \
    --dhcp-range=set:pxe,192.168.55.50,192.168.55.150,12h \
    --dhcp-vendorclass=set:pxe,PXEClient \
    --dhcp-option-force=tag:pxe,66,192.168.55.2
# D8 client classification by MAC wildcard and an explicit ignore rule.
dtest "D8 --dhcp-mac match + --dhcp-host ignore + --dhcp-ignore tag" 0 \
    --dhcp-range=192.168.55.50,192.168.55.150,12h \
    --dhcp-mac=set:aabb,00:aa:bb:*:*:* \
    --dhcp-host=00:11:22:33:44:55,ignore --dhcp-ignore=tag:aabb
# D9 reservation-only range, sequential allocation and an infinite lease.
dtest "D9 --dhcp-range static + --dhcp-sequential-ip + infinite lease" 0 \
    --dhcp-range=192.168.55.50,static,255.255.255.0 --dhcp-sequential-ip \
    --dhcp-host=de:ad:be:ef:00:01,192.168.55.201,infinite
# D10 negative: a malformed --dhcp-option (unknown option name) is rejected by the parser.
dtest "D10 malformed --dhcp-option rejected" 1 \
    --dhcp-range=192.168.55.50,192.168.55.150,12h --dhcp-option=option:definitely-not-an-option,x

######################## SECTION E: CONFIG FILE / DIR ########################
# Records loaded from a config file and a config directory are served for real over loopback.
echo "=== E. conf-file / conf-dir ==="

# E1 --conf-file: an address record read from a file resolves.
cat > "$WORK/e1.conf" <<EOF
port=$DP
listen-address=127.0.0.1
bind-interfaces
no-resolv
no-hosts
address=/fromfile.test/10.11.12.13
EOF
E1PID="$("$DM" -k --log-facility=- -u root -g root -C "$WORK/e1.conf" --pid-file="$WORK/e1.pid" > "$WORK/e1.log" 2>&1 & echo $!)"
if wait_dns host.fromfile.test 10.11.12.13; then ok 1 "E1 --conf-file loaded record resolves (10.11.12.13)"; else ok 0 "E1 --conf-file"; tail -6 "$WORK/e1.log"; fi
kill "$E1PID" 2>/dev/null; kill -9 "$E1PID" 2>/dev/null; sleep 1

# E2 --conf-dir: an address record read from a directory of *.conf resolves.
mkdir -p "$WORK/confd"
printf 'address=/fromdir.test/10.7.7.7\n' > "$WORK/confd/records.conf"
E2PID="$(start_dns "$WORK/e2.log" -p "$DP" --listen-address=127.0.0.1 --no-hosts \
    --conf-dir="$WORK/confd" --pid-file="$WORK/e2.pid")"
if wait_dns node.fromdir.test 10.7.7.7; then ok 1 "E2 --conf-dir loaded record resolves (10.7.7.7)"; else ok 0 "E2 --conf-dir"; tail -6 "$WORK/e2.log"; fi
kill "$E2PID" 2>/dev/null; kill -9 "$E2PID" 2>/dev/null; sleep 1

######################## SECTION F: INTEGRATED TFTP (REAL TRANSFER) ########################
# dnsmasq's integrated TFTP server (--enable-tftp) is driven end to end: a real tftp-hpa
# client fetches files over 127.0.0.1:69 and the bytes are verified. TFTP is pure UDP, so a
# full transfer round-trips single-node over loopback. DNS is kept on a high loopback port so
# the loopback interface stays a listener and dnsmasq binds TFTP on 127.0.0.1:69.
echo "=== F. integrated TFTP: real client transfer over 127.0.0.1:69 ==="

mkdir -p "$WORK/tftproot"
printf 'boot-payload-line\n' > "$WORK/tftproot/boot.txt"
# A multi-block payload (> 512-byte TFTP block) exercises the block/ACK loop, not just RRQ.
i=0; : > "$WORK/tftproot/big.bin"
while [ "$i" -lt 64 ]; do printf 'tftp-block-payload-%03d-0123456789abcdef\n' "$i" >> "$WORK/tftproot/big.bin"; i=$((i + 1)); done

FTPID="$("$DM" -k --log-facility=- --log-debug -u root -g root -C /dev/null \
    -p "$TDP" --listen-address=127.0.0.1 --bind-interfaces --no-resolv \
    --enable-tftp=lo --tftp-root="$WORK/tftproot" --tftp-no-fail \
    --pid-file="$WORK/ftp.pid" > "$WORK/ftp.log" 2>&1 & echo $!)"
sleep 2

if kill -0 "$FTPID" 2>/dev/null && grep -q "TFTP root is $WORK/tftproot" "$WORK/ftp.log"; then
    ok 1 "F1 --enable-tftp server up (root=$WORK/tftproot)"; FTUP=1
else
    ok 0 "F1 --enable-tftp server up"; tail -8 "$WORK/ftp.log"; FTUP=0
fi

tget() { # tget <remote> <local> : one-shot tftp GET over loopback
    rm -f "$2"
    $TFTP -m octet 127.0.0.1 69 -c get "$1" "$2" > "$WORK/tftp.cli" 2>&1
}

if [ "$FTUP" = 1 ]; then
    # F2 single-block GET: fetch boot.txt and byte-compare with the served file.
    tget boot.txt "$WORK/boot.got"
    if [ -f "$WORK/boot.got" ] && cmp -s "$WORK/tftproot/boot.txt" "$WORK/boot.got"; then
        ok 1 "F2 tftp GET single block, bytes match ($(wc -c < "$WORK/boot.got") B)"
    else
        ok 0 "F2 tftp GET single block"; cat "$WORK/tftp.cli"
    fi

    # F3 multi-block GET: fetch the > 512-byte file and byte-compare (block/ACK loop).
    tget big.bin "$WORK/big.got"
    if [ -f "$WORK/big.got" ] && cmp -s "$WORK/tftproot/big.bin" "$WORK/big.got"; then
        ok 1 "F3 tftp GET multi block, bytes match ($(wc -c < "$WORK/big.got") B)"
    else
        ok 0 "F3 tftp GET multi block (got:[$(wc -c < "$WORK/big.got" 2>/dev/null)] want:[$(wc -c < "$WORK/tftproot/big.bin")])"; cat "$WORK/tftp.cli"
    fi

    # F4 negative: a missing file is rejected (server sends an error, no local file written).
    tget no-such-file.bin "$WORK/miss.got"
    if [ ! -s "$WORK/miss.got" ] && grep -qi 'not found\|error' "$WORK/tftp.cli"; then
        ok 1 "F4 tftp GET missing file rejected"
    else
        ok 0 "F4 tftp missing file not rejected"; cat "$WORK/tftp.cli"; ls -l "$WORK/miss.got" 2>/dev/null
    fi
else
    for a in F2 F3 F4; do ok 0 "$a (tftp server down)"; done
fi
echo "--- DIAG ftp.log ---"; cat "$WORK/ftp.log" 2>/dev/null; echo "--- DIAG ss ---"; (ss -uln 2>/dev/null || netstat -uln 2>/dev/null) | grep -E ':69|:5354' | head
kill "$FTPID" 2>/dev/null; kill -9 "$FTPID" 2>/dev/null; sleep 1

######################## SECTION G: INTEGRATION ########################
# The isolated carpets above are the prerequisite. This is the end-to-end COMBINATION: one
# edge instance simultaneously serves /etc/hosts names, its own local records and forwards a
# delegated zone to an upstream - the way a real dnsmasq resolver is deployed.
echo "=== G. integration: hosts + local records + forwarding in one instance ==="

cat > "$WORK/int.hosts" <<EOF
10.20.30.40 gateway.home
EOF
IUP="$(start_dns "$WORK/iup.log" -p "$UPP" --listen-address=127.0.0.1 --no-hosts \
    --address=/isp.test/203.0.113.5 --local-ttl=1800 --pid-file="$WORK/iup.pid")"
IFW="$(start_dns "$WORK/ifw.log" -p "$DP" --listen-address=127.0.0.1 --no-hosts \
    --addn-hosts="$WORK/int.hosts" --domain=home --expand-hosts \
    --txt-record=motd.home,"welcome" --host-record=nas.home,10.20.30.50 \
    --server=/isp.test/127.0.0.1#$UPP --log-queries --pid-file="$WORK/ifw.pid")"
sleep 2

IUF="$(cat "$WORK/iup.pid" 2>/dev/null | tr -d '\r\n')"
IFF="$(cat "$WORK/ifw.pid" 2>/dev/null | tr -d '\r\n')"
if [ -n "$IUF" ] && kill -0 "$IUF" 2>/dev/null && [ -n "$IFF" ] && kill -0 "$IFF" 2>/dev/null; then
    ok 1 "G1 integrated edge instance + upstream up"; GUP=1
else
    ok 0 "G1 integrated instance up"; tail -6 "$WORK/ifw.log"; tail -6 "$WORK/iup.log"; GUP=0
fi

if [ "$GUP" = 1 ]; then
    # G2 a local /etc/hosts name AND a forwarded upstream name both resolve through it.
    RH="$(a_of gateway.home)"; RF="$(a_of www.isp.test)"
    if [ "$RH" = 10.20.30.40 ] && [ "$RF" = 203.0.113.5 ]; then
        ok 1 "G2 hosts name + forwarded name via one instance (10.20.30.40 / 203.0.113.5)"
    else
        ok 0 "G2 integration hosts+forward (host:[$RH] fwd:[$RF])"
    fi

    # G3 a local TXT and a local host-record A both served by the same integrated instance.
    RT="$($NS -type=txt motd.home 127.0.0.1 2>/dev/null | tr -d '\r')"
    RN="$(a_of nas.home)"
    if echo "$RT" | grep -q 'text = "welcome"' && [ "$RN" = 10.20.30.50 ]; then
        ok 1 "G3 local TXT + host-record A on integrated instance (welcome / 10.20.30.50)"
    else
        ok 0 "G3 integration records (txt:[$RT] nas:[$RN])"
    fi
else
    ok 0 "G2 integration hosts+forward"; ok 0 "G3 integration records"
fi
kill "$IFW" "$IUP" 2>/dev/null; kill -9 "$IFW" "$IUP" 2>/dev/null

######################## AGGREGATE ########################
echo "AGGREGATE: PASS=$PASS TOTAL=$TOTAL EXPECTED=$EXPECTED"
if [ "$PASS" = "$TOTAL" ] && [ "$TOTAL" = "$EXPECTED" ]; then
    printf 'DNSMASQ_OK=%s/%s\n' "$PASS" "$EXPECTED"
    echo "TEST PASSED"
    exit 0
fi
printf 'DNSMASQ_OK=%s/%s\n' "$PASS" "$EXPECTED"
echo "TEST FAILED"
exit 1
