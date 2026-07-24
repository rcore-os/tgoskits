#!/bin/sh
# run-consul-etcd.sh - on-target gate for the StarryOS consul-etcd distributed-KV carpet.
#
# Staged into the rootfs by prebuild.sh and invoked as the ENTIRE shell_init_cmd
# (`sh /usr/bin/run-consul-etcd.sh`). The gate lives in a staged script, not inline in the
# toml, so the harness never echoes a literal TEST PASSED back over the serial console and
# self-matches success_regex: TEST PASSED is printed ONLY by this script, ONLY when every
# assertion of both carpets passed.
#
# Two production Go services are exercised single-node over IPv4 loopback (a StarryOS single
# VM has no second host, so multi-node raft/gossip clustering stays out of scope - it needs
# multiple VMs / network namespaces, a real wall). Both binaries are fully static CGO-free Go
# ELF, so nothing but the binary + a writable ext4 data dir is needed.
#
#   CONSUL 1.22.7 (consul agent -dev):
#     version red-line / dev agent up / members / KV put-get-recurse-keys-delete /
#     service register + catalog services + catalog nodes / health check passing /
#     snapshot save + restore.
#   ETCD 3.6.11 (single-node server):
#     etcd + etcdctl version red-line / server ready / KV put-get-del / watch event /
#     txn / lease (grant+attach+TTL+keep-alive+expiry) / member list / snapshot save.
#
# Data dirs live under /root (ext4, bounded page cache), NEVER /tmp (tmpfs, unbounded):
# both raft-boltdb (consul, when persisting) and bbolt (etcd) mmap their db with a large
# InitialMmapSize; on tmpfs that pins unbounded pages.
set -u

export PATH=/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root
CONSUL=/usr/local/bin/consul
ETCD=/usr/bin/etcd
ETCDCTL=/usr/bin/etcdctl
ETCDUTL=/usr/bin/etcdutl
export CONSUL_HTTP_ADDR=127.0.0.1:8500

ARCH="$(uname -m)"
# etcd's tier-1 gate refuses to start on riscv64/loong64 unless ETCD_UNSUPPORTED_ARCH is set
# to the GOARCH. The binary itself works - this is etcd policy, not a kernel limit.
case "$ARCH" in
    riscv64)     export ETCD_UNSUPPORTED_ARCH=riscv64 ;;
    loongarch64) export ETCD_UNSUPPORTED_ARCH=loong64 ;;
esac

PASS=0
TOTAL=0
ok() { # ok <0|1> <label>
    TOTAL=$((TOTAL + 1))
    if [ "$1" = 1 ]; then PASS=$((PASS + 1)); echo "  OK   $2"; else echo "  FAIL $2"; fi
}

############################ CONSUL ############################
echo "=== consul 1.22.7 carpet (dev agent: version/members/kv/services/health/snapshot) ==="

# 1) version red-line: exact Consul v1.22.7 (proves the static Go ELF loads + runs).
$CONSUL version > /tmp/cv.out 2>&1
if grep -q 'Consul v1.22.7' /tmp/cv.out; then ok 1 "consul version v1.22.7"; else ok 0 "consul version"; tail -3 /tmp/cv.out; fi

# single-node dev agent on loopback (embedded raft + serf LAN gossip + gRPC/HTTP/DNS).
rm -rf /root/consul.d; mkdir -p /root/consul.d
$CONSUL agent -dev -bind=127.0.0.1 -client=127.0.0.1 -node=starrynode \
    -data-dir=/root/consul.d > /tmp/agent.out 2>&1 &
APID=$!
CRDY=0; i=0
while [ $i -lt 300 ]; do
    grep -q 'Consul agent running!' /tmp/agent.out 2>/dev/null && { CRDY=1; break; }
    kill -0 "$APID" 2>/dev/null || break
    i=$((i + 1)); sleep 2
done
# 2) agent ready.
ok "$CRDY" "consul dev agent running (loopback serf+raft+http)"

if [ "$CRDY" = 1 ]; then
    sleep 3
    # 3) members: node reported alive (client RPC round-trips to the live agent).
    $CONSUL members > /tmp/mem.out 2>&1
    if grep -q 'starrynode' /tmp/mem.out && grep -q 'alive' /tmp/mem.out; then
        ok 1 "consul members: starrynode alive"; grep 'starrynode' /tmp/mem.out
    else
        ok 0 "consul members"; tail -4 /tmp/mem.out
    fi

    # 4) KV put/get byte-exact round-trip.
    $CONSUL kv put starry/k1 hello-42 > /tmp/kvput.out 2>&1
    GOT=$($CONSUL kv get starry/k1 2>/dev/null | tr -d '\r\n')
    if [ "$GOT" = "hello-42" ]; then ok 1 "consul kv put/get roundtrip=hello-42"; else ok 0 "consul kv get (got:[$GOT])"; fi
    $CONSUL kv put starry/k2 world > /dev/null 2>&1
    $CONSUL kv put starry/k3 third > /dev/null 2>&1

    # 5) KV recursive read (values of the whole prefix).
    $CONSUL kv get -recurse starry/ > /tmp/rec.out 2>&1
    if grep -q 'starry/k1:hello-42' /tmp/rec.out && grep -q 'starry/k2:world' /tmp/rec.out && grep -q 'starry/k3:third' /tmp/rec.out; then
        ok 1 "consul kv get -recurse (3 keys)"
    else
        ok 0 "consul kv get -recurse"; cat /tmp/rec.out
    fi

    # 6) KV key listing.
    $CONSUL kv get -keys starry/ > /tmp/keys.out 2>&1
    if grep -q 'starry/k1' /tmp/keys.out && grep -q 'starry/k2' /tmp/keys.out && grep -q 'starry/k3' /tmp/keys.out; then
        ok 1 "consul kv get -keys"
    else
        ok 0 "consul kv get -keys"; cat /tmp/keys.out
    fi

    # 7) KV delete: the key is gone (get exits non-zero + "No key exists").
    $CONSUL kv delete starry/k1 > /dev/null 2>&1
    $CONSUL kv get starry/k1 > /tmp/del.out 2>&1
    DRC=$?
    if [ "$DRC" != 0 ] && grep -qi 'No key exists' /tmp/del.out; then
        ok 1 "consul kv delete (get -> no key)"
    else
        ok 0 "consul kv delete (rc=$DRC)"; cat /tmp/del.out
    fi

    # 8) service registration + service-discovery catalog.
    $CONSUL services register /root/consul-etcd/consul-service.json > /tmp/reg.out 2>&1
    sleep 2
    $CONSUL catalog services > /tmp/cs.out 2>&1
    if grep -qx 'web' /tmp/cs.out; then ok 1 "consul service register + catalog services(web)"; else ok 0 "consul catalog services"; cat /tmp/cs.out; fi

    # 9) catalog nodes: the registered node is in the catalog.
    $CONSUL catalog nodes > /tmp/cn.out 2>&1
    if grep -q 'starrynode' /tmp/cn.out; then ok 1 "consul catalog nodes(starrynode)"; else ok 0 "consul catalog nodes"; cat /tmp/cn.out; fi

    # 10) health check: the web service's TCP check reaches "passing" (the health subsystem
    #     runs the check on its 3s interval; the passing-filtered watch lists service:web).
    #     The consul watch is a heavy process that on slow TCG needs several seconds just to
    #     spawn and emit, so wait for OUTPUT CONTENT (not a fixed window) before killing it,
    #     and retry until service:web shows passing.
    HPASS=0; h=0
    while [ $h -lt 6 ]; do
        : > /tmp/checks.out
        $CONSUL watch -type=checks -state=passing > /tmp/checks.out 2>&1 &
        WPID=$!
        w=0
        while [ $w -lt 25 ]; do
            sleep 1
            [ -s /tmp/checks.out ] && break
            w=$((w + 1))
        done
        sleep 1
        kill "$WPID" 2>/dev/null; kill -9 "$WPID" 2>/dev/null
        if grep -q 'service:web' /tmp/checks.out; then HPASS=1; break; fi
        h=$((h + 1)); sleep 3
    done
    if [ "$HPASS" = 1 ]; then
        ok 1 "consul health check service:web passing"
    else
        ok 0 "consul health check service:web passing"; head -30 /tmp/checks.out
    fi

    # 11) snapshot save: raft state serialized to a verified snapshot file.
    $CONSUL snapshot save /root/consul.snap > /tmp/snap.out 2>&1
    if grep -qi 'Saved' /tmp/snap.out && [ -s /root/consul.snap ]; then
        ok 1 "consul snapshot save"; cat /tmp/snap.out
    else
        ok 0 "consul snapshot save"; cat /tmp/snap.out
    fi

    # 12) snapshot restore: the saved snapshot is restored into the running server.
    $CONSUL snapshot restore /root/consul.snap > /tmp/rest.out 2>&1
    if grep -qi 'Restored snapshot' /tmp/rest.out; then ok 1 "consul snapshot restore"; else ok 0 "consul snapshot restore"; cat /tmp/rest.out; fi
else
    echo "  consul agent not ready; tail:"; tail -15 /tmp/agent.out
fi
kill "$APID" 2>/dev/null; kill -9 "$APID" 2>/dev/null
sleep 1

############################ ETCD ############################
echo "=== etcd 3.6.11 carpet (server: version/kv/watch/txn/lease/member/snapshot) ==="
EP=127.0.0.1:2379

# 1) etcd version red-line.
$ETCD --version > /tmp/ev.out 2>&1
if grep -qE '^etcd Version: 3\.6\.11$' /tmp/ev.out; then ok 1 "etcd --version 3.6.11"; else ok 0 "etcd --version"; tail -3 /tmp/ev.out; fi
# 2) etcdctl version red-line.
$ETCDCTL version > /tmp/ecv.out 2>&1
if grep -qE '^etcdctl version: 3\.6\.11$' /tmp/ecv.out; then ok 1 "etcdctl version 3.6.11"; else ok 0 "etcdctl version"; tail -3 /tmp/ecv.out; fi

# single-node server on loopback (Raft + bbolt MVCC + gRPC).
DATA=/root/etcd.d
rm -rf "$DATA"; mkdir -p "$DATA"
$ETCD --name s1 --data-dir "$DATA" \
    --listen-client-urls "http://$EP" --advertise-client-urls "http://$EP" \
    --listen-peer-urls "http://127.0.0.1:2380" --initial-advertise-peer-urls "http://127.0.0.1:2380" \
    --initial-cluster "s1=http://127.0.0.1:2380" --initial-cluster-token t1 --initial-cluster-state new \
    --force-new-cluster --log-level warn > /tmp/etcd.out 2>&1 &
EPID=$!
ERDY=0; i=0
while [ $i -lt 120 ]; do
    if $ETCDCTL --endpoints="$EP" --command-timeout=2s endpoint health > /tmp/eh.out 2>&1; then
        grep -q 'is healthy' /tmp/eh.out && { ERDY=1; break; }
    fi
    kill -0 "$EPID" 2>/dev/null || break
    i=$((i + 1)); sleep 2
done
# 3) server ready.
ok "$ERDY" "etcd server ready (loopback client RPC)"

if [ "$ERDY" = 1 ]; then
    # 4) KV put/get byte-exact round-trip.
    $ETCDCTL --endpoints="$EP" put foo bar42 > /tmp/eput.out 2>&1
    EGOT=$($ETCDCTL --endpoints="$EP" get foo --print-value-only 2>/tmp/eget.err | tr -d '\r\n')
    if [ "$EGOT" = "bar42" ]; then ok 1 "etcd kv put/get roundtrip=bar42"; else ok 0 "etcd kv get (got:[$EGOT])"; tail -3 /tmp/eget.err; fi

    # 5) KV delete: key removed (get returns empty).
    $ETCDCTL --endpoints="$EP" del foo > /tmp/edel.out 2>&1
    EDG=$($ETCDCTL --endpoints="$EP" get foo --print-value-only 2>/dev/null | tr -d '\r\n')
    if [ -z "$EDG" ]; then ok 1 "etcd kv del (get -> empty)"; else ok 0 "etcd kv del (still:[$EDG])"; fi

    # 6) watch: a background watcher receives the PUT event delivered by the server.
    $ETCDCTL --endpoints="$EP" watch watchkey > /tmp/watch.out 2>&1 &
    WPID=$!
    sleep 2
    $ETCDCTL --endpoints="$EP" put watchkey EVENT_123 > /dev/null 2>&1
    sleep 2
    kill "$WPID" 2>/dev/null; kill -9 "$WPID" 2>/dev/null
    if grep -q 'EVENT_123' /tmp/watch.out && grep -q 'PUT' /tmp/watch.out; then
        ok 1 "etcd watch received PUT event"
    else
        ok 0 "etcd watch"; cat /tmp/watch.out
    fi

    # 7) txn: a guarded transaction takes the success branch and applies its writes.
    $ETCDCTL --endpoints="$EP" put cnt 100 > /dev/null 2>&1
    $ETCDCTL --endpoints="$EP" txn > /tmp/txn.out 2>&1 <<'TXN'
value("cnt") = "100"

put cnt 200

put cnt 999

TXN
    TGOT=$($ETCDCTL --endpoints="$EP" get cnt --print-value-only 2>/dev/null | tr -d '\r\n')
    if grep -q 'SUCCESS' /tmp/txn.out && [ "$TGOT" = "200" ]; then
        ok 1 "etcd txn (guard true -> success branch, cnt=200)"
    else
        ok 0 "etcd txn (cnt:[$TGOT])"; cat /tmp/txn.out
    fi

    # 8) lease: grant a lease, attach a key to it, key readable while lease is live; TTL shows.
    GR=$($ETCDCTL --endpoints="$EP" lease grant 100 2>/tmp/lg.err)
    echo "  lease grant: $GR"
    # "lease <hexid> granted with TTL(100s)"
    set -- $GR; LID="${2:-}"
    if [ -n "${LID:-}" ]; then
        $ETCDCTL --endpoints="$EP" put leasekey withlease --lease="$LID" > /dev/null 2>&1
        LGET=$($ETCDCTL --endpoints="$EP" get leasekey --print-value-only 2>/dev/null | tr -d '\r\n')
        $ETCDCTL --endpoints="$EP" lease keep-alive --once "$LID" > /tmp/lka.out 2>&1
        $ETCDCTL --endpoints="$EP" lease timetolive "$LID" > /tmp/lttl.out 2>&1
        if [ "$LGET" = "withlease" ] && grep -qi 'keepalived' /tmp/lka.out && grep -qi 'remaining' /tmp/lttl.out; then
            ok 1 "etcd lease grant+attach+keep-alive+TTL"
        else
            ok 0 "etcd lease (get:[$LGET])"; cat /tmp/lka.out /tmp/lttl.out
        fi
    else
        ok 0 "etcd lease grant"; cat /tmp/lg.err
    fi

    # 9) lease TTL expiry: a key on a short lease with no keep-alive is auto-removed.
    #     Grant a generous TTL so the pre-expiry read reliably races ahead of it even on
    #     slow TCG (each etcdctl call costs seconds), then POLL for the key to disappear
    #     rather than sleeping a fixed amount.
    GR2=$($ETCDCTL --endpoints="$EP" lease grant 30 2>/dev/null)
    set -- $GR2; LID2="${2:-}"
    if [ -n "${LID2:-}" ]; then
        $ETCDCTL --endpoints="$EP" put ephkey ephval --lease="$LID2" > /dev/null 2>&1
        EPRE=$($ETCDCTL --endpoints="$EP" get ephkey --print-value-only 2>/dev/null | tr -d '\r\n')
        EXP=0; j=0
        while [ $j -lt 40 ]; do
            sleep 3
            EPOST=$($ETCDCTL --endpoints="$EP" get ephkey --print-value-only 2>/dev/null | tr -d '\r\n')
            [ -z "$EPOST" ] && { EXP=1; break; }
            j=$((j + 1))
        done
        if [ "$EPRE" = "ephval" ] && [ "$EXP" = 1 ]; then
            ok 1 "etcd lease TTL expiry (ephkey auto-removed)"
        else
            ok 0 "etcd lease expiry (pre:[$EPRE] expired:$EXP)"
        fi
    else
        ok 0 "etcd lease grant (short)"
    fi

    # 10) member list: the single member is present and started.
    $ETCDCTL --endpoints="$EP" member list > /tmp/ml.out 2>&1
    if grep -q 'started' /tmp/ml.out && grep -q ' s1,' /tmp/ml.out; then
        ok 1 "etcd member list (s1 started)"; cat /tmp/ml.out
    else
        ok 0 "etcd member list"; cat /tmp/ml.out
    fi

    # 11) snapshot save: the MVCC store is serialized to a snapshot file, verified by etcdutl.
    $ETCDCTL --endpoints="$EP" snapshot save /root/etcd.snap > /tmp/esnap.out 2>&1
    if [ -s /root/etcd.snap ] && $ETCDUTL snapshot status /root/etcd.snap > /tmp/esnst.out 2>&1; then
        ok 1 "etcd snapshot save (+etcdutl status)"; tail -2 /tmp/esnap.out
    else
        ok 0 "etcd snapshot save"; cat /tmp/esnap.out /tmp/esnst.out
    fi
else
    echo "  etcd server not ready; tail:"; tail -20 /tmp/etcd.out
fi
kill "$EPID" 2>/dev/null; kill -9 "$EPID" 2>/dev/null
sleep 1

############################ INTEGRATION ############################
# Isolated carpets above are the prerequisite (each daemon exercised on its own). This
# section is the COMBINATION test: consul and etcd run CONCURRENTLY on loopback (distinct
# port sets, no collision) and drive a real "service discovery + config center" workflow -
# register a service in consul, store its config in etcd, then discover the service from
# consul AND read its config back from etcd, tying the two systems into one flow. It proves
# two heavy Go daemons coexist (concurrent futex / mmap / netpoll / scheduler) and interoperate.
echo "=== integration: consul + etcd concurrent (service discovery + config center) ==="

rm -rf /root/consul.d; mkdir -p /root/consul.d
# Bring consul fully up FIRST, then start etcd alongside it. consul's DNS-server start has an
# internal deadline that the extreme single-core TCG slowness can blow if a second heavy Go
# runtime competes for the core during that window; once consul is up, etcd joins and both
# serve CONCURRENTLY for the workflow below (the coexistence claim holds - both stay alive).
$CONSUL agent -dev -bind=127.0.0.1 -client=127.0.0.1 -node=starrynode \
    -data-dir=/root/consul.d > /tmp/iagent.out 2>&1 &
IAPID=$!
ICRDY=0; i=0
while [ $i -lt 300 ]; do
    grep -q 'Consul agent running!' /tmp/iagent.out 2>/dev/null && { ICRDY=1; break; }
    kill -0 "$IAPID" 2>/dev/null || break
    i=$((i + 1)); sleep 2
done

IDATA=/root/etcd.d
rm -rf "$IDATA"; mkdir -p "$IDATA"
$ETCD --name s1 --data-dir "$IDATA" \
    --listen-client-urls "http://$EP" --advertise-client-urls "http://$EP" \
    --listen-peer-urls "http://127.0.0.1:2380" --initial-advertise-peer-urls "http://127.0.0.1:2380" \
    --initial-cluster "s1=http://127.0.0.1:2380" --initial-cluster-token t1 --initial-cluster-state new \
    --force-new-cluster --log-level warn > /tmp/ietcd.out 2>&1 &
IEPID=$!
IERDY=0; i=0
while [ $i -lt 120 ]; do
    if $ETCDCTL --endpoints="$EP" --command-timeout=2s endpoint health > /tmp/ieh.out 2>&1; then
        grep -q 'is healthy' /tmp/ieh.out && { IERDY=1; break; }
    fi
    kill -0 "$IEPID" 2>/dev/null || break
    i=$((i + 1)); sleep 2
done

# INT1: with consul still alive, etcd now serves too -> both run concurrently on loopback.
if [ "$ICRDY" = 1 ] && [ "$IERDY" = 1 ] && kill -0 "$IAPID" 2>/dev/null; then
    ok 1 "integration: consul + etcd serving concurrently on loopback"
else
    ok 0 "integration coexistence (consul:$ICRDY etcd:$IERDY)"
    tail -8 /tmp/iagent.out; tail -8 /tmp/ietcd.out
fi

if [ "$ICRDY" = 1 ] && [ "$IERDY" = 1 ]; then
    sleep 3
    SVCNAME=orders
    DSN="postgres://orders-db:5432/orders"
    # register the service in consul
    cat > /tmp/orders.json <<JSON
{ "service": { "name": "$SVCNAME", "port": 9090, "tags": ["prod"] } }
JSON
    $CONSUL services register /tmp/orders.json > /tmp/ireg.out 2>&1
    # store its config in etcd (the config center)
    $ETCDCTL --endpoints="$EP" put "config/$SVCNAME/dsn" "$DSN" > /dev/null 2>&1
    $ETCDCTL --endpoints="$EP" put "config/$SVCNAME/replicas" "3" > /dev/null 2>&1
    sleep 2

    # INT2: discover the service from consul's catalog.
    $CONSUL catalog services > /tmp/icat.out 2>&1
    if grep -qx "$SVCNAME" /tmp/icat.out; then
        ok 1 "integration: consul discovers registered service '$SVCNAME'"
    else
        ok 0 "integration consul discovery"; cat /tmp/icat.out
    fi

    # INT3: read the service config back from etcd, byte-exact + prefix listing.
    GOTDSN=$($ETCDCTL --endpoints="$EP" get "config/$SVCNAME/dsn" --print-value-only 2>/dev/null | tr -d '\r\n')
    $ETCDCTL --endpoints="$EP" get "config/$SVCNAME/" --prefix > /tmp/icfg.out 2>&1
    if [ "$GOTDSN" = "$DSN" ] && grep -q 'replicas' /tmp/icfg.out; then
        ok 1 "integration: etcd config-center round-trip (dsn+replicas)"
    else
        ok 0 "integration etcd config (dsn:[$GOTDSN])"; cat /tmp/icfg.out
    fi

    # INT4: end-to-end - use the name discovered from consul to key etcd, proving the two
    #       systems compose into a discover-then-configure flow.
    DISC=$(grep -xE 'orders' /tmp/icat.out | head -1)
    if [ "$DISC" = "$SVCNAME" ]; then
        E2E=$($ETCDCTL --endpoints="$EP" get "config/$DISC/dsn" --print-value-only 2>/dev/null | tr -d '\r\n')
    else
        E2E=""
    fi
    if [ "$DISC" = "$SVCNAME" ] && [ "$E2E" = "$DSN" ]; then
        ok 1 "integration: end-to-end discover(consul)->configure(etcd)"
    else
        ok 0 "integration end-to-end (disc:[$DISC] e2e:[$E2E])"
    fi
else
    echo "  integration skipped body: one daemon not ready"
    ok 0 "integration: consul discovers registered service"
    ok 0 "integration: etcd config-center round-trip"
    ok 0 "integration: end-to-end discover->configure"
fi
kill "$IAPID" 2>/dev/null; kill -9 "$IAPID" 2>/dev/null
kill "$IEPID" 2>/dev/null; kill -9 "$IEPID" 2>/dev/null

############################ AGGREGATE ############################
EXPECTED=27
echo "AGGREGATE: PASS=$PASS TOTAL=$TOTAL EXPECTED=$EXPECTED"
if [ "$PASS" = "$TOTAL" ] && [ "$TOTAL" = "$EXPECTED" ]; then
    printf 'CONSULETCD_OK=%s/%s\n' "$PASS" "$TOTAL"
    echo "TEST PASSED"
    exit 0
fi
printf 'CONSULETCD_OK=%s/%s\n' "$PASS" "$TOTAL"
echo "TEST FAILED"
exit 1
