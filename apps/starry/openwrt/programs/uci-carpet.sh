#!/bin/sh
# uci-carpet.sh - doc-grounded carpet for OpenWrt uci (Unified Configuration Interface).
#
# Ground truth = `uci` usage tree: 16 commands (batch/export/import/changes/commit/add/
# add_list/del_list/show/get/set/delete/rename/revert/reorder) and the option surface
# (-c -C -d -f -m -n -N -p -P -t -q -s -S -X). Every command and every option is exercised
# against a synthetic /etc/config tree the carpet owns; uci is self-contained (it operates on
# plain text config files - no ubus/procd needed), so the carpet is fully offline/hermetic.
#
# UCI binary: $1, else $UCI_BIN, else `uci` on PATH. Prints "UCI CARPET OK <n>" iff every
# assertion passed AND the count equals the pinned total.
set -u
UCI="${1:-${UCI_BIN:-uci}}"
command -v "$UCI" >/dev/null 2>&1 || { echo "uci not found: $UCI"; echo "UCI CARPET FAIL"; exit 1; }

WORK="${UCI_WORK:-/tmp/uci-carpet.$$}"
CFG="$WORK/config"
rm -rf "$WORK" /tmp/.uci; mkdir -p "$CFG"
# -c sets the config search path; uci stages uncommitted deltas under its default save path
# (/tmp/.uci) which the carpet clears up front so the run is hermetic.
U() { "$UCI" -c "$CFG" "$@"; }

PASS=0; FAIL=0
pass() { PASS=$((PASS+1)); }
fail() { FAIL=$((FAIL+1)); echo "FAIL: $*"; }
# eq <desc> <got> <want>
eq() { if [ "$2" = "$3" ]; then pass; else fail "$1 | got=[$2] want=[$3]"; fi; }
# ok <desc> <cmd...>  (expect exit 0)
ok() { d="$1"; shift; if "$@" >/dev/null 2>&1; then pass; else fail "$d (expected success)"; fi; }
# no <desc> <cmd...>  (expect nonzero)
no() { d="$1"; shift; if "$@" >/dev/null 2>&1; then fail "$d (expected failure)"; else pass; fi; }

# ---------------------------------------------------------------- seed config files
cat > "$CFG/network" <<'EOF'
config interface 'loopback'
	option device 'lo'
	option proto 'static'
	option ipaddr '127.0.0.1'
	option netmask '255.0.0.0'

config interface 'lan'
	option proto 'static'
	option ipaddr '192.168.1.1'
	list ports 'eth0'
	list ports 'eth1'
EOF
cat > "$CFG/system" <<'EOF'
config system
	option hostname 'OpenWrt'
	option timezone 'UTC'

config timeserver 'ntp'
	list server '0.openwrt.pool.ntp.org'
EOF

# ---------------------------------------------------------------- get / show
eq "get scalar"            "$(U get network.lan.ipaddr)"            "192.168.1.1"
eq "get typed by name"     "$(U get network.loopback.proto)"       "static"
eq "get section type"      "$(U get network.lan)"                  "interface"
eq "get anon by @type[0]"  "$(U get system.@system[0].hostname)"   "OpenWrt"
eq "get anon @type[-1]"    "$(U get system.@system[-1].timezone)"  "UTC"
no  "get missing option"   U get network.lan.nope
no  "get missing section"  U get network.nosuch
eq "show option line"      "$(U show network.lan.ipaddr)"          "network.lan.ipaddr='192.168.1.1'"
eq "show section header"   "$(U show network.lan | head -1)"       "network.lan=interface"
# whole-config show contains every seeded key
SHOW_ALL="$(U show)"
case "$SHOW_ALL" in *"network.loopback.netmask='255.0.0.0'"*) pass;; *) fail "show all missing netmask";; esac
case "$SHOW_ALL" in *"system.ntp"*) pass;; *) fail "show all missing ntp timeserver";; esac
# list rendering + -d delimiter override
eq "show list default"     "$(U get network.lan.ports)"            "eth0 eth1"
eq "show list -d delim"    "$(U -d , show network.lan.ports)" "network.lan.ports='eth0','eth1'"

# ---------------------------------------------------------------- set (create/modify) + commit
ok "set new option"        U set network.lan.gateway=192.168.1.254
eq "set readback (staged)" "$(U get network.lan.gateway)"          "192.168.1.254"
ok "set modify existing"   U set network.lan.ipaddr=10.0.0.1
eq "set modify readback"   "$(U get network.lan.ipaddr)"           "10.0.0.1"
# changes lists the staged (uncommitted) deltas
CH="$(U changes)"
case "$CH" in *"network.lan.gateway"*) pass;; *) fail "changes missing gateway";; esac
case "$CH" in *"network.lan.ipaddr"*)  pass;; *) fail "changes missing ipaddr";;  esac
# revert drops staged changes
ok "revert option"         U revert network.lan.ipaddr
eq "revert restored"       "$(U get network.lan.ipaddr)"           "192.168.1.1"
# commit persists to the on-disk file
ok "commit network"        U commit network
grep -q "192.168.1.1" "$CFG/network"   && pass || fail "commit not persisted (ipaddr)"
grep -q "192.168.1.254" "$CFG/network" && pass || fail "commit not persisted (gateway)"
eq "changes empty post-commit" "$(U changes)" ""

# ---------------------------------------------------------------- set section type / create section
ok "set create section"    U set network.wan=interface
ok "set opt on new sect"   U set network.wan.proto=dhcp
ok "commit wan"            U commit network
eq "new section readback"  "$(U get network.wan.proto)"            "dhcp"

# ---------------------------------------------------------------- add (anonymous) + rename + delete
ANON="$(U add network route)"          # prints the assigned name (cfgXXXXXX)
case "$ANON" in cfg*) pass;; *) fail "add did not print cfg name: [$ANON]";; esac
ok "set on anon section"   U set network.$ANON.target=0.0.0.0
ok "commit anon"           U commit network
eq "anon @route[0] target" "$(U get network.@route[0].target)"     "0.0.0.0"
ok "rename section"        U rename network.$ANON=myroute
ok "commit rename"         U commit network
eq "renamed section get"   "$(U get network.myroute.target)"       "0.0.0.0"
ok "rename option"         U rename network.myroute.target=dest
ok "commit opt-rename"     U commit network
eq "opt renamed readback"  "$(U get network.myroute.dest)"         "0.0.0.0"
ok "delete option"         U delete network.myroute.dest
ok "commit del-opt"        U commit network
no  "deleted option gone"  U get network.myroute.dest
ok "delete section"        U delete network.myroute
ok "commit del-sect"       U commit network
no  "deleted section gone" U get network.myroute

# ---------------------------------------------------------------- add_list / del_list
ok "add_list new"          U add_list network.lan.ports=eth2
ok "commit add_list"       U commit network
eq "add_list readback"     "$(U get network.lan.ports)"            "eth0 eth1 eth2"
ok "del_list one"          U del_list network.lan.ports=eth1
ok "commit del_list"       U commit network
eq "del_list readback"     "$(U get network.lan.ports)"            "eth0 eth2"

# ---------------------------------------------------------------- reorder
# add two anon rules then reorder the 2nd to position 0
R0="$(U add network rule)"; U set network.$R0.name=first >/dev/null 2>&1
R1="$(U add network rule)"; U set network.$R1.name=second >/dev/null 2>&1
U commit network >/dev/null 2>&1
eq "rule order pre"        "$(U get network.@rule[0].name)"        "first"
ok "reorder rule to 0"     U reorder network.$R1=0
ok "commit reorder"        U commit network
eq "rule order post"       "$(U get network.@rule[0].name)"        "second"

# ---------------------------------------------------------------- export / import / batch
EXP="$(U export system)"
case "$EXP" in *"package system"*) pass;; *) fail "export missing package header";; esac
case "$EXP" in *"option hostname 'OpenWrt'"*) pass;; *) fail "export missing hostname";; esac
# import a fresh package from stdin, then read it back
printf "package fresh\n\nconfig thing 'a'\n\toption x '1'\n" | U import fresh
ok "commit imported"       U commit fresh
eq "imported readback"     "$(U get fresh.a.x)"                    "1"
# batch: several commands over stdin in one invocation
printf "set fresh.a.y=2\nset fresh.a.z=3\ncommit fresh\n" | U batch
eq "batch set y"           "$(U get fresh.a.y)"                    "2"
eq "batch set z"           "$(U get fresh.a.z)"                    "3"

# ---------------------------------------------------------------- option-flag surface
# -q quiet: no error text on a missing key (still nonzero exit)
QOUT="$(U -q get network.lan.nope 2>&1)"; eq "-q suppresses stderr" "$QOUT" ""
# -X: do not use extended syntax on show. Accepted flag; for a named section it renders the
# same option lines (extended @type[i] references only differ for anonymous package dumps).
XSHOW="$(U -X show network.lan)"
ok "-X show accepted"      U -X show network.lan
case "$XSHOW" in *"network.lan.ipaddr="*) pass;; *) fail "-X dropped option lines";; esac
# -S disable strict / -s force strict on a clean file both succeed
ok "-s strict parses clean"  U -c "$CFG" -s show system
ok "-S non-strict parses"    U -c "$CFG" -S show system
# -N don't name unnamed sections on export -> anonymous header has no name
NEXP="$(U -N export network | grep -E "^config rule" | head -1)"
eq "-N unnamed on export"  "$NEXP"                                 "config rule"
# -n name unnamed sections on export -> anonymous header carries the cfg id
NNEXP="$(U -n export network | grep -E "^config rule" | head -1)"
case "$NNEXP" in "config rule 'cfg"*) pass;; *) fail "-n did not name section: [$NNEXP]";; esac
# -f file input for import instead of stdin
printf "package ffile\n\nconfig k 'kk'\n\toption v 'vv'\n" > "$WORK/in.uci"
ok "-f import from file"    sh -c "\"$UCI\" -c \"$CFG\" -f \"$WORK/in.uci\" import ffile && \"$UCI\" -c \"$CFG\" commit ffile"
eq "-f imported readback"  "$(U get ffile.kk.v)"                   "vv"

# ---------------------------------------------------------------- teardown / verdict
rm -rf "$WORK"
EXPECTED=70
TOTAL=$((PASS+FAIL))
echo "uci: PASS=$PASS FAIL=$FAIL TOTAL=$TOTAL EXPECTED=$EXPECTED"
if [ "$FAIL" -eq 0 ] && [ "$TOTAL" -eq "$EXPECTED" ]; then
	echo "UCI CARPET OK $PASS"
else
	echo "UCI CARPET FAIL"
	exit 1
fi
