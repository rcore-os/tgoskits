#!/bin/sh
# run-consul-etcd.sh - on-target gate for the StarryOS consul-etcd distributed-KV carpet.
#
# Staged into the rootfs by prebuild.sh and invoked as the ENTIRE shell_init_cmd
# (`sh /usr/bin/run-consul-etcd.sh`). The gate lives in a staged script, not inline in the
# toml, so the harness never echoes a literal TEST PASSED back over the serial console and
# self-matches success_regex: TEST PASSED is printed ONLY by this script, ONLY when every
# assertion of both carpets passed.
#
# The carpet has two complementary halves:
#
#   CLI SURFACE (help-tree + flag matrix, arch-independent, no daemon needed):
#     Every consul / etcdctl / etcdutl subcommand's `--help` is walked (the full command
#     tree, ground-truthed against the official v1.22.7 / v3.6.11 x86_64 releases) and each
#     subcommand's usage token is asserted present with exit code 0. Then the core commands'
#     real flag behavior is driven against the live daemons (kv put -flags/-cas, kv get
#     -detailed/-keys -separator, catalog -tags, etcdctl get --prefix/--limit/--sort-by,
#     put --prev-kv, del --prefix, lease list, member/endpoint -w json/table, ...).
#
#   RUNTIME (both daemons exercised through their real client paths, single-node loopback):
#     A StarryOS single VM has no second host, so multi-node raft/gossip clustering stays out
#     of scope - it needs multiple VMs / network namespaces, a real wall. Both binaries are
#     fully static CGO-free Go ELF, so nothing but the binary + a writable ext4 data dir is
#     needed.
#       CONSUL 1.22.7 (consul agent -dev): version / dev agent up / members / KV
#       put-get-recurse-keys-delete / service register + catalog / health check / snapshot.
#       ETCD 3.6.11 (single-node server): version / server ready / KV put-get-del / watch /
#       txn / lease (grant+attach+TTL+keep-alive+expiry) / member list / snapshot.
#
# Data dirs live under /root (ext4, bounded page cache), NEVER /tmp (tmpfs, unbounded):
# both raft-boltdb (consul, when persisting) and bbolt (etcd) mmap their db with a large
# InitialMmapSize; on tmpfs that pins unbounded pages.
set -u

export PATH=/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root
CONSUL=/usr/local/bin/consul
ETCD=/usr/bin/etcd
# The default 5s client command deadline is exceeded under slow emulated-arch TCG (aarch64 /
# riscv64 / loongarch64 run the go binaries much slower than x86_64): a fresh etcdctl process
# plus a store-wide RPC (hashkv / compaction / the first keys-only scan) can outlast it, so the
# etcdctl spec carries a wider deadline. It is kept as command-line flags rather than exported
# ETCDCTL_ env vars on purpose - exporting leaks the settings into the etcd server's own
# environment. Data-plane sites use $ETCDCTL unquoted and help_tree word-splits it, so the flags
# reach etcdctl and are never mistaken for a command path.
ETCDCTL="/usr/bin/etcdctl --command-timeout=40s --dial-timeout=20s"
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

# help_ok <binary> <label> <cmd-words...>: run `<binary> <cmd> --help`, pass iff it exits 0
# AND its output contains the literal usage token "<basename> <cmd>" (the invariant every
# cobra/mitchellh-cli subcommand prints - ground-truthed against the official releases).
help_ok() {
    bin="$1"; label="$2"; shift 2
    "$bin" "$@" --help > /tmp/h.out 2>&1
    rc=$?
    tok="$(basename "$bin") $*"
    if [ "$rc" = 0 ] && grep -qF "$tok" /tmp/h.out; then
        ok 1 "$label"
    else
        ok 0 "$label (rc=$rc)"; head -3 /tmp/h.out
    fi
}

# help_tree <binary> <label> <space-separated subcommand specs, one per line on stdin>:
# walk every subcommand's `--help`; the aggregate assertion passes only if ALL of them
# exit 0 and print their own usage token. All-or-nothing so a single regressed subcommand
# fails the whole tree (no silent partial coverage).
help_tree() {
    bin="$1"; label="$2"
    # bin may carry global flags (e.g. "etcdctl --command-timeout=40s"); the program name is the
    # first token and the invocation must word-split so the flags are passed, not run as a path.
    base="$(basename "${bin%% *}")"
    bad=""; n=0
    while IFS= read -r spec; do
        [ -z "$spec" ] && continue
        n=$((n + 1))
        # shellcheck disable=SC2086
        $bin $spec --help > /tmp/ht.out 2>&1
        rc=$?
        if [ "$rc" != 0 ] || ! grep -qF "$base $spec" /tmp/ht.out; then
            bad="$bad [$spec:rc$rc]"
        fi
    done
    if [ -z "$bad" ]; then
        ok 1 "$label ($n subcommands)"
    else
        ok 0 "$label - failing:$bad"
    fi
}

############################ CONSUL CLI SURFACE ############################
# Help tree + flag matrix are binary-carried, so they run without a daemon and are identical
# across the four arches (the cross-compiled rv64/la64 binaries are the same version).
echo "=== consul CLI surface (help tree + flag matrix) ==="

# H1) top-level `consul --help`: lists the command set (agent/kv/catalog/... present).
$CONSUL --help > /tmp/cth.out 2>&1
if [ $? = 0 ] && grep -q 'Available commands' /tmp/cth.out \
   && grep -q ' agent ' /tmp/cth.out && grep -q ' kv ' /tmp/cth.out && grep -q ' catalog ' /tmp/cth.out; then
    ok 1 "consul --help lists command set"
else
    ok 0 "consul --help"; head -5 /tmp/cth.out
fi

# H2) every top-level consul command's `--help` (the full 35-command tree).
help_tree "$CONSUL" "consul --help tree (all top-level commands)" <<'CMDS'
acl
agent
catalog
config
connect
debug
event
exec
force-leave
info
intention
join
keygen
keyring
kv
leave
lock
login
logout
maint
members
monitor
operator
peering
reload
resource
rtt
services
snapshot
tls
troubleshoot
validate
version
watch
CMDS

# H3) consul kv subcommand tree (delete/export/get/import/put).
help_tree "$CONSUL" "consul kv --help subtree" <<'CMDS'
kv delete
kv export
kv get
kv import
kv put
CMDS

# H4) consul catalog + snapshot subcommand trees.
help_tree "$CONSUL" "consul catalog + snapshot --help subtrees" <<'CMDS'
catalog datacenters
catalog nodes
catalog services
snapshot decode
snapshot inspect
snapshot restore
snapshot save
CMDS

# H5) consul ACL / connect / intention / operator surface (auth + service-mesh + operator
#     subsystems that the runtime carpet never reaches, but whose CLI must still be intact).
help_tree "$CONSUL" "consul acl/connect/intention/operator --help subtrees" <<'CMDS'
acl bootstrap
acl policy
acl policy create
acl policy list
acl token
acl token create
acl token list
acl auth-method
acl role
connect ca
connect ca get-config
connect envoy
connect proxy
intention create
intention check
intention get
intention list
operator autopilot
operator raft
operator raft list-peers
operator usage
CMDS

# H6) consul config + services + peering + resource surface.
help_tree "$CONSUL" "consul config/services/peering/resource --help subtrees" <<'CMDS'
config delete
config list
config read
config write
services register
services deregister
services export
peering list
peering read
peering generate-token
resource list
resource read
CMDS

############################ CONSUL RUNTIME ############################
echo "=== consul 1.22.7 carpet (dev agent: version/members/kv/services/health/snapshot) ==="

# 1) version red-line: exact Consul v1.22.7 (proves the static Go ELF loads + runs).
$CONSUL version > /tmp/cv.out 2>&1
if grep -q 'Consul v1.22.7' /tmp/cv.out; then ok 1 "consul version v1.22.7"; else ok 0 "consul version"; tail -3 /tmp/cv.out; fi

# single-node dev agent on loopback (embedded raft + serf LAN gossip + gRPC/HTTP).
# DNS is disabled (-dns-port=-1): this carpet exercises serf/raft/HTTP/KV/catalog/
# health/snapshot, never the DNS interface, and binding the DNS listener has a
# fixed internal startup deadline that a slow emulated-arch TCG run can miss,
# aborting the agent with "timeout starting DNS servers". Dropping the unused
# listener makes agent bring-up deterministic across arches.
rm -rf /root/consul.d; mkdir -p /root/consul.d
$CONSUL agent -dev -bind=127.0.0.1 -client=127.0.0.1 -node=starrynode \
    -dns-port=-1 -data-dir=/root/consul.d > /tmp/agent.out 2>&1 &
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

    # F1) kv put -flags=<n> stores a user flags field; kv get -detailed surfaces it.
    $CONSUL kv put -flags=42 starry/kf flagged > /dev/null 2>&1
    $CONSUL kv get -detailed starry/kf > /tmp/kvd.out 2>&1
    if grep -qE '^Flags[[:space:]]+42' /tmp/kvd.out && grep -qE '^Value[[:space:]]+flagged' /tmp/kvd.out; then
        ok 1 "consul kv put -flags + kv get -detailed (Flags 42)"
    else
        ok 0 "consul kv put -flags/-detailed"; cat /tmp/kvd.out
    fi

    # F2) kv put -cas -modify-index=0 is create-only: first write succeeds, second fails
    #     because the key now exists (Check-And-Set with index 0 means "must not exist").
    $CONSUL kv put -cas -modify-index=0 starry/kcas first > /tmp/cas1.out 2>&1; C1=$?
    $CONSUL kv put -cas -modify-index=0 starry/kcas second > /tmp/cas2.out 2>&1; C2=$?
    if [ "$C1" = 0 ] && [ "$C2" != 0 ] && grep -qi 'already exists' /tmp/cas2.out; then
        ok 1 "consul kv put -cas -modify-index=0 (create-only CAS)"
    else
        ok 0 "consul kv put -cas (rc1=$C1 rc2=$C2)"; cat /tmp/cas2.out
    fi

    # F3) kv get -keys -separator collapses a nested prefix into one folder entry.
    $CONSUL kv put starry/sub/a 1 > /dev/null 2>&1
    $CONSUL kv put starry/sub/b 2 > /dev/null 2>&1
    $CONSUL kv get -keys -separator=/ starry/ > /tmp/ksep.out 2>&1
    if grep -qx 'starry/sub/' /tmp/ksep.out; then
        ok 1 "consul kv get -keys -separator (folder collapse)"
    else
        ok 0 "consul kv get -keys -separator"; cat /tmp/ksep.out
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

    # F4) catalog services -tags appends the service's tags (web carries starry,v1).
    $CONSUL catalog services -tags > /tmp/cstags.out 2>&1
    if grep -E '^web' /tmp/cstags.out | grep -q 'starry'; then
        ok 1 "consul catalog services -tags (web tags shown)"
    else
        ok 0 "consul catalog services -tags"; cat /tmp/cstags.out
    fi

    # F5) catalog datacenters lists the local dc (dev agent defaults to dc1).
    $CONSUL catalog datacenters > /tmp/cdc.out 2>&1
    if grep -qx 'dc1' /tmp/cdc.out; then ok 1 "consul catalog datacenters (dc1)"; else ok 0 "consul catalog datacenters"; cat /tmp/cdc.out; fi

    # 9) catalog nodes: the registered node is in the catalog.
    $CONSUL catalog nodes > /tmp/cn.out 2>&1
    if grep -q 'starrynode' /tmp/cn.out; then ok 1 "consul catalog nodes(starrynode)"; else ok 0 "consul catalog nodes"; cat /tmp/cn.out; fi

    # F6) operator raft list-peers: the single dev server is the raft leader + a voter.
    $CONSUL operator raft list-peers > /tmp/orp.out 2>&1
    if grep -q 'starrynode' /tmp/orp.out && grep -q 'leader' /tmp/orp.out && grep -q 'true' /tmp/orp.out; then
        ok 1 "consul operator raft list-peers (starrynode leader/voter)"
    else
        ok 0 "consul operator raft list-peers"; cat /tmp/orp.out
    fi

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

    # F7) snapshot inspect: the saved snapshot's metadata (ID/Size/Index/Term) reads back.
    $CONSUL snapshot inspect /root/consul.snap > /tmp/sins.out 2>&1
    if grep -q 'ID' /tmp/sins.out && grep -q 'Index' /tmp/sins.out && grep -q 'Term' /tmp/sins.out; then
        ok 1 "consul snapshot inspect (metadata)"
    else
        ok 0 "consul snapshot inspect"; cat /tmp/sins.out
    fi

    # 12) snapshot restore: the saved snapshot is restored into the running server.
    $CONSUL snapshot restore /root/consul.snap > /tmp/rest.out 2>&1
    if grep -qi 'Restored snapshot' /tmp/rest.out; then ok 1 "consul snapshot restore"; else ok 0 "consul snapshot restore"; cat /tmp/rest.out; fi
else
    echo "  consul agent not ready; tail:"; tail -15 /tmp/agent.out
fi
kill "$APID" 2>/dev/null; kill -9 "$APID" 2>/dev/null
sleep 1

############################ ETCD CLI SURFACE ############################
echo "=== etcd/etcdctl/etcdutl CLI surface (help tree) ==="

# H7) `etcd --help` (the server binary): exits 0 and prints its flag block.
$ETCD --help > /tmp/eth.out 2>&1
if [ $? = 0 ] && grep -qi 'Usage:' /tmp/eth.out && grep -q -- '--data-dir' /tmp/eth.out; then
    ok 1 "etcd --help (server flags)"
else
    ok 0 "etcd --help"; head -5 /tmp/eth.out
fi

# H8) `etcdctl --help`: lists the grouped command set (get/put/lease/member/... present).
$ETCDCTL --help > /tmp/ecth.out 2>&1
if [ $? = 0 ] && grep -q 'COMMANDS' /tmp/ecth.out \
   && grep -q '  get' /tmp/ecth.out && grep -q '  put' /tmp/ecth.out && grep -q 'lease grant' /tmp/ecth.out; then
    ok 1 "etcdctl --help lists command set"
else
    ok 0 "etcdctl --help"; head -5 /tmp/ecth.out
fi

# H9) every etcdctl subcommand's `--help` (the full grouped command tree).
help_tree "$ETCDCTL" "etcdctl --help tree (all subcommands)" <<'CMDS'
alarm disarm
alarm list
auth disable
auth enable
auth status
check datascale
check perf
compaction
completion
defrag
del
downgrade cancel
downgrade enable
downgrade validate
elect
endpoint hashkv
endpoint health
endpoint status
get
lease grant
lease keep-alive
lease list
lease revoke
lease timetolive
lock
make-mirror
member add
member list
member promote
member remove
member update
move-leader
put
role add
role delete
role get
role grant-permission
role list
role revoke-permission
snapshot save
txn
user add
user delete
user get
user grant-role
user list
user passwd
user revoke-role
version
watch
CMDS

# H10) etcdctl auth/user/role surface: the multi-tenant auth subsystem the runtime carpet
#      never enables (a dev single node runs auth-disabled), but whose CLI must be intact.
help_tree "$ETCDCTL" "etcdctl auth/user/role --help surface" <<'CMDS'
auth enable
auth disable
auth status
user add
user get
user list
user delete
user passwd
user grant-role
user revoke-role
role add
role get
role list
role delete
role grant-permission
role revoke-permission
CMDS

# H11) `etcdutl --help` + its full subcommand tree (offline snapshot/db tooling).
$ETCDUTL --help > /tmp/euth.out 2>&1
if [ $? = 0 ] && grep -q 'Available Commands' /tmp/euth.out && grep -q 'snapshot' /tmp/euth.out; then
    ok 1 "etcdutl --help lists command set"
else
    ok 0 "etcdutl --help"; head -5 /tmp/euth.out
fi
help_tree "$ETCDUTL" "etcdutl --help tree (all subcommands)" <<'CMDS'
completion
defrag
hashkv
migrate
snapshot
snapshot restore
snapshot status
version
CMDS

############################ ETCD RUNTIME ############################
echo "=== etcd 3.6.11 carpet (server: version/kv/watch/txn/lease/member/snapshot) ==="
EP=127.0.0.1:2379

# 1) etcd version red-line.
$ETCD --version > /tmp/ev.out 2>&1
if grep -qE '^etcd Version: 3\.6\.11$' /tmp/ev.out; then ok 1 "etcd --version 3.6.11"; else ok 0 "etcd --version"; tail -3 /tmp/ev.out; fi
# 2) etcdctl version red-line.
$ETCDCTL version > /tmp/ecv.out 2>&1
if grep -qE '^etcdctl version: 3\.6\.11$' /tmp/ecv.out; then ok 1 "etcdctl version 3.6.11"; else ok 0 "etcdctl version"; tail -3 /tmp/ecv.out; fi

# single-node server on loopback (Raft + bbolt MVCC + gRPC). --unsafe-no-fsync drops the WAL
# fsync: on emulated-arch TCG the virtio-blk fsync latency makes the boot-time raft member
# publish exceed its 7s deadline (server.go "failed to publish local member ... context deadline
# exceeded"), which leaves the server stuck sub-ready; an ephemeral single-node test needs no
# on-disk durability. The wider heartbeat/election also lifts the derived publish deadline.
DATA=/root/etcd.d
rm -rf "$DATA"; mkdir -p "$DATA"
$ETCD --name s1 --data-dir "$DATA" \
    --listen-client-urls "http://$EP" --advertise-client-urls "http://$EP" \
    --listen-peer-urls "http://127.0.0.1:2380" --initial-advertise-peer-urls "http://127.0.0.1:2380" \
    --initial-cluster "s1=http://127.0.0.1:2380" --initial-cluster-token t1 --initial-cluster-state new \
    --force-new-cluster --unsafe-no-fsync --heartbeat-interval=500 --election-timeout=5000 \
    --log-level warn > /tmp/etcd.out 2>&1 &
EPID=$!
ERDY=0; i=0
# endpoint health is a linearizable read; on emulated-arch TCG that round-trip alone can take
# several seconds, so a 2s probe deadline false-negatives a server that is in fact serving and
# skips the whole etcd suite. A port that is not up yet still fails fast (connection refused).
while [ $i -lt 120 ]; do
    if $ETCDCTL --endpoints="$EP" --command-timeout=15s endpoint health > /tmp/eh.out 2>&1; then
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

    # F8) put --prev-kv returns the prior key/value the write replaced.
    $ETCDCTL --endpoints="$EP" put foo newval --prev-kv > /tmp/eprev.out 2>&1
    if grep -q 'foo' /tmp/eprev.out && grep -q 'bar42' /tmp/eprev.out; then
        ok 1 "etcd put --prev-kv (returns replaced foo=bar42)"
    else
        ok 0 "etcd put --prev-kv"; cat /tmp/eprev.out
    fi

    # 5) KV delete: key removed (get returns empty).
    $ETCDCTL --endpoints="$EP" del foo > /tmp/edel.out 2>&1
    EDG=$($ETCDCTL --endpoints="$EP" get foo --print-value-only 2>/dev/null | tr -d '\r\n')
    if [ -z "$EDG" ]; then ok 1 "etcd kv del (get -> empty)"; else ok 0 "etcd kv del (still:[$EDG])"; fi

    # F9) get --prefix flag matrix: seed a 3-key prefix, then exercise --keys-only, --limit,
    #     --sort-by/--order and the -w json count in one grouped assertion (all must hold).
    $ETCDCTL --endpoints="$EP" put pfx/a A1 > /dev/null 2>&1
    $ETCDCTL --endpoints="$EP" put pfx/b B2 > /dev/null 2>&1
    $ETCDCTL --endpoints="$EP" put pfx/c C3 > /dev/null 2>&1
    KO=$($ETCDCTL --endpoints="$EP" get pfx/ --prefix --keys-only 2>/dev/null | grep -c '^pfx/')
    LIM=$($ETCDCTL --endpoints="$EP" get pfx/ --prefix --keys-only --limit 2 2>/dev/null | grep -c '^pfx/')
    FIRST=$($ETCDCTL --endpoints="$EP" get pfx/ --prefix --keys-only --sort-by=KEY --order=DESCEND 2>/dev/null | grep '^pfx/' | head -1)
    CNT=$($ETCDCTL --endpoints="$EP" get pfx/ --prefix -w json 2>/dev/null | grep -o '"count":3')
    if [ "$KO" = 3 ] && [ "$LIM" = 2 ] && [ "$FIRST" = "pfx/c" ] && [ "$CNT" = '"count":3' ]; then
        ok 1 "etcd get --prefix/--keys-only/--limit/--sort-by/-w json"
    else
        ok 0 "etcd get --prefix matrix (ko=$KO lim=$LIM first=$FIRST cnt=$CNT)"
    fi

    # F10) del --prefix returns the count of keys it removed (all three).
    DELN=$($ETCDCTL --endpoints="$EP" del pfx/ --prefix 2>/dev/null | tr -d '\r\n')
    if [ "$DELN" = 3 ]; then ok 1 "etcd del --prefix (deleted 3)"; else ok 0 "etcd del --prefix (got:[$DELN])"; fi

    # 6) watch: a background watcher receives a PUT event delivered by the server. The watcher
    #    is established asynchronously - etcdctl connects and opens the stream before any event
    #    is observable - so a single fixed pre-put sleep can let the PUT race ahead of stream
    #    setup on slow TCG, and the watcher then never sees it. Put repeatedly until the watcher
    #    captures an event (bounded); once the stream is up, the next PUT is delivered. This polls
    #    the real signal instead of a fixed delay, matching the consul-watch loop above.
    $ETCDCTL --endpoints="$EP" watch watchkey > /tmp/watch.out 2>&1 &
    WPID=$!
    wgot=0
    for _ in $(seq 1 20); do
        $ETCDCTL --endpoints="$EP" put watchkey EVENT_123 > /dev/null 2>&1
        sleep 1
        if grep -q 'EVENT_123' /tmp/watch.out; then wgot=1; break; fi
    done
    kill "$WPID" 2>/dev/null; kill -9 "$WPID" 2>/dev/null
    if [ "$wgot" = 1 ] && grep -q 'PUT' /tmp/watch.out; then
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

    # F11) txn mod() compare: a revision-guard transaction takes the success branch too
    #      (distinct guard operator from the value() guard above).
    $ETCDCTL --endpoints="$EP" put mk base > /dev/null 2>&1
    $ETCDCTL --endpoints="$EP" txn > /tmp/mtxn.out 2>&1 <<'MTXN'
mod("mk") > "0"

put mk winner

put mk loser

MTXN
    MGOT=$($ETCDCTL --endpoints="$EP" get mk --print-value-only 2>/dev/null | tr -d '\r\n')
    if grep -q 'SUCCESS' /tmp/mtxn.out && [ "$MGOT" = "winner" ]; then
        ok 1 "etcd txn mod() guard (success branch, mk=winner)"
    else
        ok 0 "etcd txn mod() guard (mk:[$MGOT])"; cat /tmp/mtxn.out
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
        # F12) lease list reports the live lease (both the count line and the hex id).
        $ETCDCTL --endpoints="$EP" lease list > /tmp/llist.out 2>&1
        if grep -qi 'found' /tmp/llist.out && grep -q "$LID" /tmp/llist.out; then
            ok 1 "etcd lease list (live lease reported)"
        else
            ok 0 "etcd lease list"; cat /tmp/llist.out
        fi
    else
        ok 0 "etcd lease grant"; cat /tmp/lg.err
        ok 0 "etcd lease list (no lease)"
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

    # F13) member list -w json exposes the machine-readable members array.
    $ETCDCTL --endpoints="$EP" member list -w json > /tmp/mljson.out 2>&1
    if grep -q '"members"' /tmp/mljson.out; then
        ok 1 "etcd member list -w json (members array)"
    else
        ok 0 "etcd member list -w json"; head -c 200 /tmp/mljson.out
    fi

    # F14) endpoint status -w table renders the status table with its column header.
    $ETCDCTL --endpoints="$EP" endpoint status -w table > /tmp/estat.out 2>&1
    if grep -q 'ENDPOINT' /tmp/estat.out && grep -q 'IS LEADER' /tmp/estat.out; then
        ok 1 "etcd endpoint status -w table (header)"
    else
        ok 0 "etcd endpoint status -w table"; cat /tmp/estat.out
    fi

    # F15) endpoint hashkv returns the KV-history hash for the endpoint.
    $ETCDCTL --endpoints="$EP" endpoint hashkv > /tmp/ehash.out 2>&1
    if grep -qE '127\.0\.0\.1:2379, [0-9]+' /tmp/ehash.out; then
        ok 1 "etcd endpoint hashkv (hash reported)"
    else
        ok 0 "etcd endpoint hashkv"; cat /tmp/ehash.out
    fi

    # F16) alarm list on a healthy store is empty and exits 0 (no NOSPACE/CORRUPT alarms).
    $ETCDCTL --endpoints="$EP" alarm list > /tmp/alarm.out 2>&1
    ALRC=$?
    if [ "$ALRC" = 0 ] && [ ! -s /tmp/alarm.out ]; then
        ok 1 "etcd alarm list (no alarms)"
    else
        ok 0 "etcd alarm list (rc=$ALRC)"; cat /tmp/alarm.out
    fi

    # F17) compaction: put a key, read back its revision, then compact history to it.
    $ETCDCTL --endpoints="$EP" put ckey cval > /dev/null 2>&1
    CREV=$($ETCDCTL --endpoints="$EP" get ckey -w json 2>/dev/null | grep -o '"mod_revision":[0-9]*' | head -1 | grep -o '[0-9]*')
    if [ -n "$CREV" ]; then
        $ETCDCTL --endpoints="$EP" compaction "$CREV" > /tmp/comp.out 2>&1
        if grep -q "compacted revision $CREV" /tmp/comp.out; then
            ok 1 "etcd compaction (history compacted to rev $CREV)"
        else
            ok 0 "etcd compaction"; cat /tmp/comp.out
        fi
    else
        ok 0 "etcd compaction (no revision read)"
    fi

    # 11) snapshot save: the MVCC store is serialized to a snapshot file, verified by etcdutl.
    $ETCDCTL --endpoints="$EP" snapshot save /root/etcd.snap > /tmp/esnap.out 2>&1
    if [ -s /root/etcd.snap ] && $ETCDUTL snapshot status /root/etcd.snap > /tmp/esnst.out 2>&1; then
        ok 1 "etcd snapshot save (+etcdutl status)"; tail -2 /tmp/esnap.out
    else
        ok 0 "etcd snapshot save"; cat /tmp/esnap.out /tmp/esnst.out
    fi

    # F18) etcdutl snapshot status -w table renders the offline snapshot metadata table.
    $ETCDUTL snapshot status -w table /root/etcd.snap > /tmp/esst.out 2>&1
    if grep -q 'HASH' /tmp/esst.out && grep -q 'REVISION' /tmp/esst.out && grep -q 'TOTAL KEYS' /tmp/esst.out; then
        ok 1 "etcdutl snapshot status -w table (metadata table)"
    else
        ok 0 "etcdutl snapshot status -w table"; cat /tmp/esst.out
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
    -dns-port=-1 -data-dir=/root/consul.d > /tmp/iagent.out 2>&1 &
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
    --force-new-cluster --unsafe-no-fsync --heartbeat-interval=500 --election-timeout=5000 \
    --log-level warn > /tmp/ietcd.out 2>&1 &
IEPID=$!
IERDY=0; i=0
# Same slow-arch linearizable-read tolerance as the standalone etcd probe above.
while [ $i -lt 120 ]; do
    if $ETCDCTL --endpoints="$EP" --command-timeout=15s endpoint health > /tmp/ieh.out 2>&1; then
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
EXPECTED=57
echo "AGGREGATE: PASS=$PASS TOTAL=$TOTAL EXPECTED=$EXPECTED"
if [ "$PASS" = "$TOTAL" ] && [ "$TOTAL" = "$EXPECTED" ]; then
    printf 'CONSULETCD_OK=%s/%s\n' "$PASS" "$TOTAL"
    echo "TEST PASSED"
    exit 0
fi
printf 'CONSULETCD_OK=%s/%s\n' "$PASS" "$TOTAL"
echo "TEST FAILED"
exit 1
