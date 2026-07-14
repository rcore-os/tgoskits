#!/bin/sh
# run-dropbear.sh - on-target gate for the StarryOS dropbear SSH carpet.
#
# Staged into the rootfs by prebuild.sh and invoked as the ENTIRE shell_init_cmd
# (`sh /usr/bin/run-dropbear.sh`). The gate lives in a staged script, not inline in the
# toml, so the harness never echoes a literal TEST PASSED back over the serial console and
# self-matches success_regex: TEST PASSED is printed ONLY by this script, ONLY when every
# assertion passed AND the count equals the pinned EXPECTED total (a skipped or silently
# dropped assertion changes TOTAL and fails the gate).
#
# dropbear is a small musl-dynamic SSH suite. The apk `dropbear` package ships the server
# (/usr/sbin/dropbear) + dropbearkey; the client (dbclient), format converter
# (dropbearconvert), scp and the ssh->dbclient symlink live in the dropbear-dbclient /
# -convert / -scp / -ssh subpackages. Provisioning is on-target `apk add` against the
# branch that matches the running rootfs (apk resolves the CURRENT version - no pinned,
# drifting URL); when the binaries are already present (host chroot pre-flight) the apk
# step is skipped and the very same carpet runs unchanged.
#
# The carpet drives every component and every option a single loopback host can reach:
#   A  each binary self-certifies its identity + full option surface (incl. the forwarding
#      surface: dbclient offers -L/-R/-B, and dropbear has no dynamic-SOCKS -D).
#   B  dropbearkey generates every key type this build ships (rsa/ecdsa/ed25519 across the
#      size sweep), reads the pubkey + SHA256 fingerprint back, and rejects invalid types -
#      including `dss`, which Alpine dropbear 2025.88 is built WITHOUT (DSA is removed
#      upstream: no dss.c, no ssh-dss hostkey algorithm in the binaries).
#   C  the server runs single-node and dbclient runs REAL key-authenticated sessions per key
#      type with cipher/MAC/pty/port/pidfile/forced-command options and the negative paths.
#   D  dropbearconvert round-trips rsa/ecdsa/ed25519 keys through the OpenSSH format and the
#      converted keys authenticate for real.
#   E  end-to-end keygen -> server -> auth -> remote exec -> scp file transfer.
#   F  port forwarding: -L local, -R remote and -B netcat-alike all carry real service bytes,
#      -g gateway ports, and the server -j/-k forwarding lockouts are enforced.
set -u

export PATH=/usr/local/bin:/usr/bin:/usr/sbin:/bin:/sbin
export HOME=/root
WORK="${DROPBEAR_WORK:-/root/dropbear-carpet}"
EMPTY="$WORK/emptyhome"          # a HOME with no ~/.ssh/id_dropbear, so -i offers ONLY the named key
KDIR="$WORK/keys"
HKEY="$WORK/hostkey_ed25519"
AUTH="$HOME/.ssh/authorized_keys"

DBEAR="${DB_DROPBEAR:-dropbear}"     # server (resolved on PATH: /usr/sbin/dropbear)
DBKEY="${DB_DROPBEARKEY:-dropbearkey}"
DBCLI="${DB_DBCLIENT:-dbclient}"
DBCONV="${DB_DROPBEARCONVERT:-dropbearconvert}"
DBSCP="${DB_SCP:-scp}"
DBSSH="${DB_SSH:-ssh}"

# Distinct loopback ports so short-lived server instances never collide.
MP=22001      # main server (key auth / options / scp / convert)
WP=22002      # -w disallow-root server
AP=22003      # -p address:port bound server
CP=22006      # -c forced-command server
FP=22010      # forwarding endpoint server (also the forward target: -> 127.0.0.1:FP)
JP=22011      # -j disable-local-forwarding server
KP=22012      # -k disable-remote-forwarding server

rm -rf "$WORK"; mkdir -p "$WORK" "$EMPTY" "$KDIR" "$HOME/.ssh"; : > "$AUTH"; chmod 700 "$HOME/.ssh"; chmod 600 "$AUTH"

# Every assertion below must be accounted for; a drift between TOTAL and EXPECTED is a failure.
EXPECTED=50

PASS=0
TOTAL=0
ok() { # ok <0|1> <label>
    TOTAL=$((TOTAL + 1))
    if [ "$1" = 1 ]; then PASS=$((PASS + 1)); echo "  OK   $2"; else echo "  FAIL $2"; fi
}

# ---------------------------------------------------------------------------------------
# Provision dropbear via on-target apk add, matching the rootfs Alpine branch. Skipped when
# every binary is already resolvable (host chroot pre-flight, or a warm rootfs).
have_all() {
    command -v "$DBEAR" >/dev/null 2>&1 && command -v "$DBKEY" >/dev/null 2>&1 && \
    command -v "$DBCLI" >/dev/null 2>&1 && command -v "$DBCONV" >/dev/null 2>&1 && \
    command -v "$DBSCP" >/dev/null 2>&1
}

apk_branch() {
    if [ -n "${DROPBEAR_APK_BRANCH:-}" ]; then printf '%s\n' "$DROPBEAR_APK_BRANCH"; return; fi
    rel=""; [ -r /etc/alpine-release ] && rel="$(cat /etc/alpine-release 2>/dev/null)"
    maj="$(printf '%s' "$rel" | cut -d. -f1)"; min="$(printf '%s' "$rel" | cut -d. -f2)"
    if [ -n "$maj" ] && [ -n "$min" ]; then printf 'v%s.%s\n' "$maj" "$min"; else printf 'latest-stable\n'; fi
}

apk_add_from() {  # apk_add_from <mirror>
    _m="$1"; _b="$(apk_branch)"
    cat > "$WORK/repositories" <<EOF
$_m/$_b/main
$_m/$_b/community
EOF
    echo "  apk: $_m/$_b"
    timeout "${DROPBEAR_APK_TIMEOUT:-180}" apk --no-progress --update-cache \
        --repositories-file "$WORK/repositories" add \
        dropbear dropbear-dbclient dropbear-convert dropbear-scp dropbear-ssh \
        > "$WORK/apk.log" 2>&1
}

provision() {
    if have_all; then echo "=== provision: dropbear suite already present, skipping apk ==="; return 0; fi
    echo "=== provision: apk add dropbear suite (branch-matched, current version) ==="
    for m in "${DROPBEAR_APK_MIRROR:-https://mirrors.tuna.tsinghua.edu.cn/alpine}" \
             "https://dl-cdn.alpinelinux.org/alpine"; do
        if apk_add_from "$m" && have_all; then echo "  apk add OK"; return 0; fi
        tail -6 "$WORK/apk.log" 2>/dev/null
    done
    echo "  apk add FAILED on every mirror"; return 1
}

# start_server <port> <extra-args...> : launch a foreground/stderr dropbear on a loopback
# port with the shared ed25519 hostkey; echoes the pid. Caller waits for readiness + kills.
start_server() {
    _p="$1"; shift
    "$DBEAR" -F -E -p "127.0.0.1:$_p" -r "$HKEY" "$@" > "$WORK/dbear-$_p.log" 2>&1 &
    echo $!
}

# probe <port> <keyfile> : run one BatchMode key-auth session, echo the remote token on
# success (empty on failure). Uses EMPTY HOME so ONLY <keyfile> is offered.
probe() {
    HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$2" -p "$1" root@127.0.0.1 \
        "echo DBSESS_$3" 2>/dev/null
}

# wait_ready <port> <keyfile> : poll until a probe session round-trips, or give up.
wait_ready() {
    _i=0
    while [ "$_i" -lt 30 ]; do
        [ "$(probe "$1" "$2" rdy 2>/dev/null | tr -d '\r\n')" = "DBSESS_rdy" ] && return 0
        _i=$((_i + 1)); sleep 1
    done
    return 1
}

# tcp_read <host> <port> : connect and print whatever the peer sends first. dropbear emits
# its "SSH-2.0-dropbear_..." identification banner immediately on connect, so this reads the
# banner of whatever real service a forward lands on. The sleep keeps stdin open long enough
# for the banner to arrive; timeout bounds the whole read.
tcp_read() {
    { sleep 2; } | timeout 6 nc "$1" "$2" 2>/dev/null
}

# read_forward <listenport> : retry tcp_read against a just-established forward until the
# dropbear banner comes back through it (proving the forward carried real service bytes).
read_forward() {
    _i=0
    while [ "$_i" -lt 12 ]; do
        _out="$(tcp_read 127.0.0.1 "$1")"
        case "$_out" in *SSH-2.0-dropbear*) printf '%s' "$_out"; return 0 ;; esac
        _i=$((_i + 1)); sleep 1
    done
    return 1
}

provision || { echo "PROVISION FAILED"; echo "DROPBEAR_OK=0/$EXPECTED"; echo "TEST FAILED"; exit 1; }

######################## SECTION A: BINARY SELF-CERTIFICATION ########################
# Every provisioned binary proves it loads and runs by reporting its identity + usage tree.
echo "=== A. binary self-certification (dropbear/dropbearkey/dbclient/dropbearconvert/scp/ssh) ==="

VER="$("$DBEAR" -V 2>&1 | tr -d '\r')"
if echo "$VER" | grep -q 'Dropbear v20'; then ok 1 "A1 dropbear -V ($VER)"; else ok 0 "A1 dropbear -V"; echo "$VER"; fi

"$DBEAR" -h > "$WORK/dh.out" 2>&1
if grep -q -- '-r keyfile' "$WORK/dh.out" && grep -q -- '-p \[address:\]port' "$WORK/dh.out" && \
   grep -q -- '-F' "$WORK/dh.out" && grep -q -- '-E' "$WORK/dh.out" && grep -q -- '-w' "$WORK/dh.out" && \
   grep -q -- '-s' "$WORK/dh.out" && grep -q -- '-g' "$WORK/dh.out" && grep -q -- '-P PidFile' "$WORK/dh.out" && \
   grep -q -- '-j' "$WORK/dh.out" && grep -q -- '-k' "$WORK/dh.out" && grep -q -- '-a' "$WORK/dh.out" && \
   grep -q -- '-c command' "$WORK/dh.out" && \
   grep -q -- '-W' "$WORK/dh.out" && grep -q -- '-K' "$WORK/dh.out" && grep -q -- '-I' "$WORK/dh.out"; then
    ok 1 "A2 dropbear -h enumerates server option surface (incl. -j/-k/-a/-c forwarding+cmd)"
else
    ok 0 "A2 dropbear -h"; tail -8 "$WORK/dh.out"
fi

"$DBKEY" > "$WORK/dkh.out" 2>&1
if grep -q 'rsa' "$WORK/dkh.out" && grep -q 'ecdsa' "$WORK/dkh.out" && grep -q 'ed25519' "$WORK/dkh.out" && \
   grep -q -- '-t type' "$WORK/dkh.out" && grep -q -- '-s bits' "$WORK/dkh.out" && grep -q -- '-y' "$WORK/dkh.out"; then
    ok 1 "A3 dropbearkey usage enumerates key types + options"
else
    ok 0 "A3 dropbearkey usage"; tail -8 "$WORK/dkh.out"
fi

CVER="$("$DBCLI" -V 2>&1 | tr -d '\r')"
if echo "$CVER" | grep -q 'Dropbear v20'; then ok 1 "A4 dbclient -V ($CVER)"; else ok 0 "A4 dbclient -V"; echo "$CVER"; fi

"$DBCLI" 2>&1 | tr -d '\r' > "$WORK/dch.out"
if grep -q 'Dropbear SSH client' "$WORK/dch.out" && grep -q -- '-i <identityfile>' "$WORK/dch.out" && \
   grep -q -- '-p <remoteport>' "$WORK/dch.out" && grep -q -- '-o option' "$WORK/dch.out" && \
   grep -q -- '-N' "$WORK/dch.out" && grep -q -- '-c <cipher list>' "$WORK/dch.out" && \
   grep -q -- '-m <MAC list>' "$WORK/dch.out"; then
    ok 1 "A5 dbclient usage enumerates client option surface"
else
    ok 0 "A5 dbclient usage"; tail -10 "$WORK/dch.out"
fi

"$DBCONV" 2>&1 | tr -d '\r' > "$WORK/cvh.out"
if grep -q 'inputtype.*outputtype.*inputfile.*outputfile' "$WORK/cvh.out" && \
   grep -q '^openssh' "$WORK/cvh.out" && grep -q '^dropbear' "$WORK/cvh.out"; then
    ok 1 "A6 dropbearconvert usage enumerates openssh/dropbear types"
else
    ok 0 "A6 dropbearconvert usage"; tail -8 "$WORK/cvh.out"
fi

# dropbear scp is the dropbear-scp subpackage's scp; -h / bad invocation prints its usage.
"$DBSCP" 2>&1 | tr -d '\r' > "$WORK/scph.out"
if grep -qiE 'usage: *scp' "$WORK/scph.out" || grep -qi 'scp' "$WORK/scph.out"; then
    ok 1 "A7 scp (dropbear-scp) self-reports usage"
else
    ok 0 "A7 scp usage"; tail -6 "$WORK/scph.out"
fi

# ssh is the dropbear-ssh symlink to dbclient - it must report the Dropbear SSH client banner.
"$DBSSH" 2>&1 | tr -d '\r' > "$WORK/sshh.out"
if grep -q 'Dropbear SSH client' "$WORK/sshh.out"; then
    ok 1 "A8 ssh -> dropbear dbclient (Dropbear SSH client banner)"
else
    ok 0 "A8 ssh symlink"; tail -6 "$WORK/sshh.out"
fi

# A9 dbclient forwarding surface: -L (local), -R (remote) and -B (netcat-alike) are offered;
# dropbear has NO dynamic-SOCKS -D (it was never implemented in dbclient), so it MUST be
# absent from the usage. This records the real forwarding surface exercised in section F.
if grep -q -- '-L <' "$WORK/dch.out" && grep -q -- '-R <' "$WORK/dch.out" && \
   grep -q -- '-B <endhost:endport>' "$WORK/dch.out" && ! grep -qE '^-D ' "$WORK/dch.out"; then
    ok 1 "A9 dbclient forwarding surface (-L/-R/-B present, no dynamic-SOCKS -D)"
else
    ok 0 "A9 dbclient forwarding surface"; grep -E -- '-[LRBD] ' "$WORK/dch.out"
fi

######################## SECTION B: DROPBEARKEY ########################
# Every key type this build ships across the size sweep is generated, then the public key +
# SHA256 fingerprint read back with -y; the comment (-C), an invalid-type rejection and the
# DSS-absence rejection are checked too.
echo "=== B. dropbearkey (rsa/ecdsa/ed25519 size sweep, pubkey read, fingerprint) ==="

# B1 rsa 2048
"$DBKEY" -t rsa -s 2048 -f "$KDIR/rsa2048" > "$WORK/b1.out" 2>&1
if [ -s "$KDIR/rsa2048" ] && grep -qi '2048 bit rsa' "$WORK/b1.out"; then ok 1 "B1 dropbearkey rsa 2048"; else ok 0 "B1 dropbearkey rsa 2048"; cat "$WORK/b1.out"; fi

# B2 rsa 3072 (proves the -s size sweep beyond the default)
"$DBKEY" -t rsa -s 3072 -f "$KDIR/rsa3072" > "$WORK/b2.out" 2>&1
if [ -s "$KDIR/rsa3072" ] && grep -qi '3072 bit rsa' "$WORK/b2.out"; then ok 1 "B2 dropbearkey rsa 3072"; else ok 0 "B2 dropbearkey rsa 3072"; cat "$WORK/b2.out"; fi

# B3 ecdsa 256
"$DBKEY" -t ecdsa -s 256 -f "$KDIR/ec256" > "$WORK/b3.out" 2>&1
if [ -s "$KDIR/ec256" ] && grep -qi '256 bit ecdsa' "$WORK/b3.out"; then ok 1 "B3 dropbearkey ecdsa 256"; else ok 0 "B3 dropbearkey ecdsa 256"; cat "$WORK/b3.out"; fi

# B4 ecdsa 384
"$DBKEY" -t ecdsa -s 384 -f "$KDIR/ec384" > "$WORK/b4.out" 2>&1
if [ -s "$KDIR/ec384" ] && grep -qi '384 bit ecdsa' "$WORK/b4.out"; then ok 1 "B4 dropbearkey ecdsa 384"; else ok 0 "B4 dropbearkey ecdsa 384"; cat "$WORK/b4.out"; fi

# B5 ecdsa 521
"$DBKEY" -t ecdsa -s 521 -f "$KDIR/ec521" > "$WORK/b5.out" 2>&1
if [ -s "$KDIR/ec521" ] && grep -qi '521 bit ecdsa' "$WORK/b5.out"; then ok 1 "B5 dropbearkey ecdsa 521"; else ok 0 "B5 dropbearkey ecdsa 521"; cat "$WORK/b5.out"; fi

# B6 ed25519 (fixed 256)
"$DBKEY" -t ed25519 -f "$KDIR/ed" > "$WORK/b6.out" 2>&1
if [ -s "$KDIR/ed" ] && grep -qi 'ed25519 key' "$WORK/b6.out"; then ok 1 "B6 dropbearkey ed25519"; else ok 0 "B6 dropbearkey ed25519"; cat "$WORK/b6.out"; fi

# B7 -y read ed25519 pubkey + SHA256 fingerprint
"$DBKEY" -y -f "$KDIR/ed" > "$WORK/b7.out" 2>&1
if grep -q '^ssh-ed25519 ' "$WORK/b7.out" && grep -q 'Fingerprint: SHA256:' "$WORK/b7.out"; then
    ok 1 "B7 dropbearkey -y ed25519 pubkey + SHA256 fingerprint"
else
    ok 0 "B7 dropbearkey -y ed25519"; cat "$WORK/b7.out"
fi

# B8 -y read ecdsa pubkey (nistp curve identifier)
"$DBKEY" -y -f "$KDIR/ec384" > "$WORK/b8.out" 2>&1
if grep -q '^ecdsa-sha2-nistp384 ' "$WORK/b8.out" && grep -q 'Fingerprint: SHA256:' "$WORK/b8.out"; then
    ok 1 "B8 dropbearkey -y ecdsa pubkey (nistp384)"
else
    ok 0 "B8 dropbearkey -y ecdsa"; cat "$WORK/b8.out"
fi

# B9 -y read rsa pubkey
"$DBKEY" -y -f "$KDIR/rsa2048" > "$WORK/b9.out" 2>&1
if grep -q '^ssh-rsa ' "$WORK/b9.out" && grep -q 'Fingerprint: SHA256:' "$WORK/b9.out"; then
    ok 1 "B9 dropbearkey -y rsa pubkey"
else
    ok 0 "B9 dropbearkey -y rsa"; cat "$WORK/b9.out"
fi

# B10 -C comment is embedded in the generated public key line
"$DBKEY" -t ed25519 -C carpet@starry -f "$KDIR/edc" > "$WORK/b10.out" 2>&1
if grep -q 'carpet@starry' "$WORK/b10.out"; then ok 1 "B10 dropbearkey -C comment in pubkey"; else ok 0 "B10 dropbearkey -C comment"; cat "$WORK/b10.out"; fi

# B11 invalid key type is rejected (exception path)
"$DBKEY" -t bogustype -f "$KDIR/bad" > "$WORK/b11.out" 2>&1
BRC=$?
if [ "$BRC" != 0 ] && [ ! -s "$KDIR/bad" ]; then ok 1 "B11 dropbearkey rejects invalid type (rc=$BRC)"; else ok 0 "B11 dropbearkey invalid type (rc=$BRC)"; cat "$WORK/b11.out"; fi

# B12 dss/DSA is REMOVED from this dropbear build: `-t dss` is rejected, no key is written,
# and the usage does not list dss (only rsa/ecdsa/ed25519). This documents, on-target, that
# there is no DSS host key to generate - the algorithm is gone upstream.
"$DBKEY" -t dss -f "$KDIR/dss" > "$WORK/b12.out" 2>&1
DRC=$?
if [ "$DRC" != 0 ] && [ ! -s "$KDIR/dss" ] && ! grep -qiw 'dss' "$WORK/dkh.out"; then
    ok 1 "B12 dropbearkey -t dss rejected (DSS not built; usage lists rsa/ecdsa/ed25519 only)"
else
    ok 0 "B12 dropbearkey dss (rc=$DRC)"; cat "$WORK/b12.out"
fi

######################## SECTION C: DROPBEAR SERVICE + DBCLIENT ########################
# The server is brought up single-node on loopback; dbclient runs REAL key-authenticated
# sessions per key type, exercises cipher/MAC/option selection + a forced command, and the
# negative paths (wrong key, -w root lockout) are rejected.
echo "=== C. dropbear service + dbclient real SSH sessions ==="

"$DBKEY" -t ed25519 -f "$HKEY" > /dev/null 2>&1   # shared hostkey
# register the ed25519/rsa/ecdsa client pubkeys as authorized
for k in ed rsa2048 ec384; do
    "$DBKEY" -y -f "$KDIR/$k" 2>/dev/null | grep -E '^(ssh-|ecdsa-)' >> "$AUTH"
done
chmod 600 "$AUTH"

MPID="$(start_server "$MP" -P "$WORK/dropbear.pid" -W 65536 -K 300 -I 0)"

# C1 service up + accepting connections on loopback (probe session round-trips)
if wait_ready "$MP" "$KDIR/ed"; then
    RDYLOG=0; grep -q 'Not backgrounding' "$WORK/dbear-$MP.log" 2>/dev/null && RDYLOG=1
    ok 1 "C1 dropbear service up + accepting loopback SSH (-F backgrounding=$RDYLOG)"
    CUP=1
else
    ok 0 "C1 dropbear service up"; tail -8 "$WORK/dbear-$MP.log"; CUP=0
fi

if [ "$CUP" = 1 ]; then
    # C2 ed25519 key auth: isolated -i (empty HOME) so ONLY the ed25519 key can authenticate.
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/ed" -p "$MP" root@127.0.0.1 "echo ED_OK_\$(id -u)" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "ED_OK_0" ]; then ok 1 "C2 dbclient ed25519 key auth + remote exec"; else ok 0 "C2 dbclient ed25519 (got:[$R])"; fi

    # C3 rsa key auth
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/rsa2048" -p "$MP" root@127.0.0.1 "echo RSA_OK" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "RSA_OK" ]; then ok 1 "C3 dbclient rsa key auth + remote exec"; else ok 0 "C3 dbclient rsa (got:[$R])"; fi

    # C4 ecdsa key auth
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/ec384" -p "$MP" root@127.0.0.1 "echo ECDSA_OK" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "ECDSA_OK" ]; then ok 1 "C4 dbclient ecdsa key auth + remote exec"; else ok 0 "C4 dbclient ecdsa (got:[$R])"; fi

    # C5 remote command with arguments is executed byte-exact (uname -s == Linux)
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/ed" -p "$MP" root@127.0.0.1 "uname -s" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "Linux" ]; then ok 1 "C5 dbclient remote command with args (uname -s=Linux)"; else ok 0 "C5 dbclient remote args (got:[$R])"; fi

    # C6 -p explicit remote port form: user@host with -p selects the port
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/ed" -p "$MP" root@127.0.0.1 "echo PORT_OK" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "PORT_OK" ]; then ok 1 "C6 dbclient -p explicit port"; else ok 0 "C6 dbclient -p (got:[$R])"; fi

    # C7 -c cipher selection: force aes256-ctr and the session still succeeds
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -c aes256-ctr -i "$KDIR/ed" -p "$MP" root@127.0.0.1 "echo CIPHER_OK" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "CIPHER_OK" ]; then ok 1 "C7 dbclient -c aes256-ctr cipher session"; else ok 0 "C7 dbclient -c cipher (got:[$R])"; fi

    # C8 -m MAC selection: force hmac-sha2-256 and the session still succeeds
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -m hmac-sha2-256 -i "$KDIR/ed" -p "$MP" root@127.0.0.1 "echo MAC_OK" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "MAC_OK" ]; then ok 1 "C8 dbclient -m hmac-sha2-256 session"; else ok 0 "C8 dbclient -m MAC (got:[$R])"; fi

    # C9 -T (no pty) forced: a non-interactive command session still returns output
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -T -i "$KDIR/ed" -p "$MP" root@127.0.0.1 "echo NOPTY_OK" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "NOPTY_OK" ]; then ok 1 "C9 dbclient -T (no pty) command session"; else ok 0 "C9 dbclient -T (got:[$R])"; fi

    # C10 wrong key is REJECTED (empty HOME + BatchMode: only the unregistered key is offered)
    "$DBKEY" -t ed25519 -f "$KDIR/wrong" > /dev/null 2>&1
    WR=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/wrong" -p "$MP" root@127.0.0.1 "echo SHOULD_NOT_APPEAR" 2>"$WORK/c10.err")
    WRC=$?
    if [ "$WRC" != 0 ] && [ "$(echo "$WR" | tr -d '\r\n')" != "SHOULD_NOT_APPEAR" ] && grep -qi 'auth' "$WORK/c10.err"; then
        ok 1 "C10 dbclient wrong key rejected (rc=$WRC, no auth methods)"
    else
        ok 0 "C10 wrong key rejection (rc=$WRC got:[$WR])"; tail -2 "$WORK/c10.err"
    fi

    # C11 -P pidfile written by the server carries its live pid
    PF="$(cat "$WORK/dropbear.pid" 2>/dev/null | tr -d '\r\n')"
    if [ -n "$PF" ] && kill -0 "$PF" 2>/dev/null; then ok 1 "C11 dropbear -P pidfile ($PF live)"; else ok 0 "C11 dropbear -P pidfile (pf:[$PF])"; fi
else
    ok 0 "C2 ed25519 auth"; ok 0 "C3 rsa auth"; ok 0 "C4 ecdsa auth"; ok 0 "C5 remote args"
    ok 0 "C6 -p port"; ok 0 "C7 -c cipher"; ok 0 "C8 -m mac"; ok 0 "C9 -T nopty"
    ok 0 "C10 wrong key"; ok 0 "C11 pidfile"
fi
kill "$MPID" 2>/dev/null; kill -9 "$MPID" 2>/dev/null; sleep 1

# C12 -w disallow-root: a server started with -w must REJECT the (root) login
WPID="$(start_server "$WP" -w)"
sleep 3
WOUT=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/ed" -p "$WP" root@127.0.0.1 "echo ROOT_GOT_IN" 2>"$WORK/c12.err")
if [ "$(echo "$WOUT" | tr -d '\r\n')" != "ROOT_GOT_IN" ]; then
    ok 1 "C12 dropbear -w disallow-root rejects root login"
else
    ok 0 "C12 -w disallow-root (got:[$WOUT])"; tail -3 "$WORK/c12.err"
fi
kill "$WPID" 2>/dev/null; kill -9 "$WPID" 2>/dev/null; sleep 1

# C13 -p address:port binds a specific loopback address; a session on it round-trips
APID="$(start_server "$AP")"
if wait_ready "$AP" "$KDIR/ed"; then
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/ed" -p "$AP" root@127.0.0.1 "echo ADDR_OK" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "ADDR_OK" ]; then ok 1 "C13 dropbear -p 127.0.0.1:$AP bound session"; else ok 0 "C13 -p address:port (got:[$R])"; fi
else
    ok 0 "C13 -p address:port server up"; tail -6 "$WORK/dbear-$AP.log"
fi
kill "$APID" 2>/dev/null; kill -9 "$APID" 2>/dev/null; sleep 1

# C14 -c forced command: a server started with -c runs THAT command for every session,
# overriding whatever the client asked to run.
CPID="$(start_server "$CP" -c "echo FORCED_CMD_OK")"
CR=""; _i=0
while [ "$_i" -lt 25 ]; do
    CR=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/ed" -p "$CP" root@127.0.0.1 "echo CLIENT_ASKED_THIS" 2>/dev/null | tr -d '\r\n')
    [ -n "$CR" ] && break
    _i=$((_i + 1)); sleep 1
done
if [ "$CR" = "FORCED_CMD_OK" ]; then ok 1 "C14 dropbear -c forced command overrides client command"; else ok 0 "C14 -c forced command (got:[$CR])"; tail -4 "$WORK/dbear-$CP.log"; fi
kill "$CPID" 2>/dev/null; kill -9 "$CPID" 2>/dev/null; sleep 1

######################## SECTION D: DROPBEARCONVERT ########################
# A dropbear key of every type is round-tripped through the OpenSSH format and back with
# dropbearconvert; the round-tripped public half must be identical and it must authenticate.
echo "=== D. dropbearconvert (dropbear <-> openssh format, rsa/ecdsa/ed25519) ==="

# D1 dropbear -> openssh
"$DBCONV" dropbear openssh "$KDIR/ec384" "$WORK/ec384.ossh" > "$WORK/d1.out" 2>&1
if grep -qi 'Wrote key' "$WORK/d1.out" && head -1 "$WORK/ec384.ossh" 2>/dev/null | grep -q 'BEGIN OPENSSH PRIVATE KEY'; then
    ok 1 "D1 dropbearconvert dropbear->openssh (PEM emitted)"
else
    ok 0 "D1 dropbearconvert dropbear->openssh"; cat "$WORK/d1.out"; head -1 "$WORK/ec384.ossh" 2>/dev/null
fi

# D2 openssh -> dropbear (of D1's output)
"$DBCONV" openssh dropbear "$WORK/ec384.ossh" "$WORK/ec384.rt" > "$WORK/d2.out" 2>&1
if grep -qi 'Wrote key' "$WORK/d2.out" && [ -s "$WORK/ec384.rt" ]; then
    ok 1 "D2 dropbearconvert openssh->dropbear (round-trip back)"
else
    ok 0 "D2 dropbearconvert openssh->dropbear"; cat "$WORK/d2.out"
fi

# D3 the round-tripped key's public half is identical to the original
PA="$("$DBKEY" -y -f "$KDIR/ec384" 2>/dev/null | grep '^ecdsa-')"
PB="$("$DBKEY" -y -f "$WORK/ec384.rt" 2>/dev/null | grep '^ecdsa-')"
if [ -n "$PA" ] && [ "$PA" = "$PB" ]; then ok 1 "D3 converted ecdsa public half preserved"; else ok 0 "D3 converted pubkey mismatch"; fi

# D4 the converted (round-tripped) ecdsa key authenticates a REAL session
DPID="$(start_server 22004)"
if wait_ready 22004 "$WORK/ec384.rt"; then
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$WORK/ec384.rt" -p 22004 root@127.0.0.1 "echo CONVERTED_OK" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "CONVERTED_OK" ]; then ok 1 "D4 converted ecdsa key authenticates a real session"; else ok 0 "D4 converted key auth (got:[$R])"; fi
else
    ok 0 "D4 converted key server up"
fi
kill "$DPID" 2>/dev/null; kill -9 "$DPID" 2>/dev/null; sleep 1

# D5 rsa round-trip: dropbear->openssh->dropbear, public half preserved (reuses B1's rsa2048)
"$DBCONV" dropbear openssh "$KDIR/rsa2048" "$WORK/rsa.ossh" > "$WORK/d5a.out" 2>&1
"$DBCONV" openssh dropbear "$WORK/rsa.ossh" "$WORK/rsa.rt" > "$WORK/d5b.out" 2>&1
RA="$("$DBKEY" -y -f "$KDIR/rsa2048" 2>/dev/null | grep '^ssh-rsa')"
RB="$("$DBKEY" -y -f "$WORK/rsa.rt" 2>/dev/null | grep '^ssh-rsa')"
if [ -s "$WORK/rsa.rt" ] && [ -n "$RA" ] && [ "$RA" = "$RB" ]; then ok 1 "D5 dropbearconvert rsa round-trip (public half preserved)"; else ok 0 "D5 rsa round-trip"; cat "$WORK/d5a.out" "$WORK/d5b.out"; fi

# D6 ed25519 round-trip: dropbear->openssh->dropbear, public half preserved (reuses B6's ed)
"$DBCONV" dropbear openssh "$KDIR/ed" "$WORK/ed.ossh" > "$WORK/d6a.out" 2>&1
"$DBCONV" openssh dropbear "$WORK/ed.ossh" "$WORK/ed.rt" > "$WORK/d6b.out" 2>&1
EA="$("$DBKEY" -y -f "$KDIR/ed" 2>/dev/null | grep '^ssh-ed25519')"
EB="$("$DBKEY" -y -f "$WORK/ed.rt" 2>/dev/null | grep '^ssh-ed25519')"
if [ -s "$WORK/ed.rt" ] && [ -n "$EA" ] && [ "$EA" = "$EB" ]; then ok 1 "D6 dropbearconvert ed25519 round-trip (public half preserved)"; else ok 0 "D6 ed25519 round-trip"; cat "$WORK/d6a.out" "$WORK/d6b.out"; fi

######################## SECTION E: INTEGRATION ########################
# The isolated carpets above are the prerequisite. This is the end-to-end COMBINATION: a
# freshly generated key pair drives a full keygen -> server -> authenticated session ->
# remote exec -> scp file transfer chain, the way a real SSH deployment is used.
echo "=== E. integration: keygen -> service -> auth -> exec -> scp ==="

rm -f "$AUTH"; : > "$AUTH"; chmod 600 "$AUTH"
"$DBKEY" -t ed25519 -f "$WORK/int_host" > /dev/null 2>&1
"$DBKEY" -t rsa -s 2048 -f "$KDIR/int_cli" > /dev/null 2>&1
"$DBKEY" -y -f "$KDIR/int_cli" 2>/dev/null | grep '^ssh-rsa' >> "$AUTH"
chmod 600 "$AUTH"
"$DBEAR" -F -E -p "127.0.0.1:22005" -r "$WORK/int_host" > "$WORK/int.log" 2>&1 &
IPID=$!

if wait_ready 22005 "$KDIR/int_cli"; then
    ok 1 "E1 integration server up (fresh host+client key pair)"
    # E2 authenticated remote exec returns an exact multi-token payload
    R=$(HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/int_cli" -p 22005 root@127.0.0.1 "echo INT_\$(id -u)_\$(uname -s)" 2>/dev/null | tr -d '\r\n')
    if [ "$R" = "INT_0_Linux" ]; then ok 1 "E2 integration authenticated remote exec (INT_0_Linux)"; else ok 0 "E2 integration exec (got:[$R])"; fi
    # E3 scp a real file over the dropbear link, verify the payload byte-exact
    echo "SCP_PAYLOAD_carpet_42" > "$WORK/scp_src.txt"
    rm -f "$WORK/scp_dst.txt"
    HOME="$EMPTY" "$DBSCP" -i "$KDIR/int_cli" -P 22005 -o StrictHostKeyChecking=no \
        "$WORK/scp_src.txt" root@127.0.0.1:"$WORK/scp_dst.txt" > "$WORK/e3.out" 2>&1
    if [ "$(cat "$WORK/scp_dst.txt" 2>/dev/null | tr -d '\r\n')" = "SCP_PAYLOAD_carpet_42" ]; then
        ok 1 "E3 integration scp file transfer over dropbear link"
    else
        ok 0 "E3 integration scp"; tail -3 "$WORK/e3.out"
    fi
else
    ok 0 "E1 integration server up"; tail -6 "$WORK/int.log"
    ok 0 "E2 integration exec"; ok 0 "E3 integration scp"
fi
kill "$IPID" 2>/dev/null; kill -9 "$IPID" 2>/dev/null; sleep 1

######################## SECTION F: PORT FORWARDING ########################
# dropbear's real forwarding surface, exercised end to end: -L local, -R remote and -B
# netcat-alike each carry the live SSH banner of a real service (a second dropbear on FP)
# through the tunnel; -g opens the forward to non-loopback; and the server -j/-k lockouts
# are asserted to actually block local/remote forwarding. dropbear has no dynamic-SOCKS -D
# (see A9), so there is nothing to forward through a SOCKS proxy.
echo "=== F. port forwarding (-L / -R / -B / -g, server -j/-k lockouts) ==="

# Re-authorize the ed25519 client key (section E cleared authorized_keys) + forward endpoint.
"$DBKEY" -y -f "$KDIR/ed" 2>/dev/null | grep '^ssh-ed25519' >> "$AUTH"; chmod 600 "$AUTH"
FPID="$(start_server "$FP")"
if command -v nc >/dev/null 2>&1 && wait_ready "$FP" "$KDIR/ed"; then FUP=1; else FUP=0; fi

if [ "$FUP" = 1 ]; then
    # F1 -L local port forwarding: a local listener on 22020 tunnels to 127.0.0.1:FP.
    HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -N -y -y -i "$KDIR/ed" \
        -L 22020:127.0.0.1:"$FP" -p "$FP" root@127.0.0.1 > "$WORK/f1.log" 2>&1 &
    L1=$!; sleep 4
    if read_forward 22020 | grep -q 'SSH-2.0-dropbear'; then ok 1 "F1 dbclient -L local forward carries service banner"; else ok 0 "F1 -L local forward"; tail -3 "$WORK/f1.log"; fi
    kill "$L1" 2>/dev/null; kill -9 "$L1" 2>/dev/null

    # F2 -R remote port forwarding: the SERVER listens on 22021 and tunnels back to the client
    # which connects to 127.0.0.1:FP.
    HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -N -y -y -i "$KDIR/ed" \
        -R 22021:127.0.0.1:"$FP" -p "$FP" root@127.0.0.1 > "$WORK/f2.log" 2>&1 &
    L2=$!; sleep 4
    if read_forward 22021 | grep -q 'SSH-2.0-dropbear'; then ok 1 "F2 dbclient -R remote forward carries service banner"; else ok 0 "F2 -R remote forward"; tail -3 "$WORK/f2.log"; fi
    kill "$L2" 2>/dev/null; kill -9 "$L2" 2>/dev/null

    # F3 -B netcat-alike: dbclient pipes stdin/stdout to 127.0.0.1:FP through the SSH server;
    # its stdout is the target's banner (like `ssh -W`).
    BR=$( { sleep 2; } | HOME="$EMPTY" timeout 8 "$DBCLI" -o BatchMode=yes -y -y -i "$KDIR/ed" \
        -B 127.0.0.1:"$FP" -p "$FP" root@127.0.0.1 2>/dev/null )
    if echo "$BR" | grep -q 'SSH-2.0-dropbear'; then ok 1 "F3 dbclient -B netcat-alike forward pipes service banner"; else ok 0 "F3 -B netcat-alike (got:[$(echo "$BR" | tr -d '\r\n' | cut -c1-40)])"; fi

    # F4 -g gateway ports: -L with -g binds the forward on all addresses and still tunnels.
    HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -N -g -y -y -i "$KDIR/ed" \
        -L 22022:127.0.0.1:"$FP" -p "$FP" root@127.0.0.1 > "$WORK/f4.log" 2>&1 &
    L4=$!; sleep 4
    if read_forward 22022 | grep -q 'SSH-2.0-dropbear'; then ok 1 "F4 dbclient -g -L gateway-ports forward"; else ok 0 "F4 -g gateway forward"; tail -3 "$WORK/f4.log"; fi
    kill "$L4" 2>/dev/null; kill -9 "$L4" 2>/dev/null
else
    ok 0 "F1 -L local forward (nc/server unavailable)"; ok 0 "F2 -R remote forward"
    ok 0 "F3 -B netcat-alike"; ok 0 "F4 -g gateway forward"
fi

# F5 server -j disables LOCAL forwarding: normal sessions still work, but a -L forward
# through the -j server yields NO banner (the direct-tcpip channel is refused).
JPID="$(start_server "$JP" -j)"
if command -v nc >/dev/null 2>&1 && wait_ready "$JP" "$KDIR/ed"; then
    HOME="$EMPTY" "$DBCLI" -o BatchMode=yes -N -y -y -i "$KDIR/ed" \
        -L 22023:127.0.0.1:"$JP" -p "$JP" root@127.0.0.1 > "$WORK/f5.log" 2>&1 &
    L5=$!; sleep 4
    JBAN="$(tcp_read 127.0.0.1 22023)"
    kill "$L5" 2>/dev/null; kill -9 "$L5" 2>/dev/null
    if ! echo "$JBAN" | grep -q 'SSH-2.0-dropbear'; then ok 1 "F5 server -j blocks local forwarding (no banner through -L)"; else ok 0 "F5 -j lockout leaked a forward"; fi
else
    ok 0 "F5 server -j (nc/server unavailable)"
fi
kill "$JPID" 2>/dev/null; kill -9 "$JPID" 2>/dev/null; sleep 1

# F6 server -k disables REMOTE forwarding: dbclient -R with ExitOnForwardFailure=yes has its
# tcpip-forward request refused, so it exits non-zero and never runs the remote command.
KPID="$(start_server "$KP" -k)"
if wait_ready "$KP" "$KDIR/ed"; then
    FR=$(HOME="$EMPTY" timeout 30 "$DBCLI" -o BatchMode=yes -o ExitOnForwardFailure=yes -y -y \
        -i "$KDIR/ed" -R 22024:127.0.0.1:"$KP" -p "$KP" root@127.0.0.1 "echo FWD_RAN" 2>/dev/null)
    FRC=$?
    if [ "$FRC" != 0 ] && [ "$(echo "$FR" | tr -d '\r\n')" != "FWD_RAN" ]; then ok 1 "F6 server -k blocks remote forwarding (tcpip-forward refused, rc=$FRC)"; else ok 0 "F6 -k lockout (rc=$FRC got:[$FR])"; fi
else
    ok 0 "F6 server -k server up"
fi
kill "$KPID" 2>/dev/null; kill -9 "$KPID" 2>/dev/null
kill "$FPID" 2>/dev/null; kill -9 "$FPID" 2>/dev/null

######################## AGGREGATE ########################
echo "AGGREGATE: PASS=$PASS TOTAL=$TOTAL EXPECTED=$EXPECTED"
if [ "$PASS" = "$TOTAL" ] && [ "$TOTAL" = "$EXPECTED" ]; then
    printf 'DROPBEAR_OK=%s/%s\n' "$PASS" "$EXPECTED"
    echo "TEST PASSED"
    exit 0
fi
printf 'DROPBEAR_OK=%s/%s\n' "$PASS" "$EXPECTED"
echo "TEST FAILED"
exit 1
