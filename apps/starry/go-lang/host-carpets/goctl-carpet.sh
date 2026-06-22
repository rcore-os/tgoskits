#!/usr/bin/env bash
# goctl-carpet.sh — shell-style deterministic carpet for the goctl CLI.
#
# Mirrors main.go (the canonical Go carpet) as a portable shell script. Covers
# the full goctl subcommand/flag tree for the pinned version (goctl 1.10.1).
# Two PRIMARY E2E flows (api go -> serve HTTP -> curl; rpc protoc -> serve grpc
# -> client.Ping) plus every other generator/flag, each exercised or printed as
# SKIP-with-reason.
#
# Output: one `ok: <label> = <value>` line per assertion + GOCTL_COUNT=<n>.
# Deterministic: fixed module names (generated under fixed-named subdirs), fixed
# loopback ports, sorted file lists, no timestamps/addresses/random in output.
#
# ROBUSTNESS (the whole point of run()/run_out below):
#   * EVERY external command (goctl, go mod tidy, go build, curl, the server
#     binary) is routed through a `timeout` wrapper so the script can NEVER hang.
#     run()  -> rc-only, default GOCTL_TIMEOUT (180s) budget.
#     run_e2e() -> the slow build/tidy steps, longer GOCTL_E2E_TIMEOUT (300s).
#     `timeout` exits 124 on expiry; we treat 124 as the rc, so the
#     `ok: <label>.rc = false` line RECORDS the timeout instead of hanging.
#   * Every `kill $SRV` is followed by a timeout-bounded reap (reap()) — no bare
#     `wait $SRV` that could block forever.
set -u

# ---- pinned env (overridable) ----
# GO_TOOLCHAIN_ROOT must contain go/ (go1.26.3) and a goctl 1.10.1 binary on PATH
# (e.g. under bin/ or gopath/bin/). Defaults to the dev machine layout; override
# on other hosts:  GO_TOOLCHAIN_ROOT=/path/to/go-toolchain bash goctl-carpet.sh
TC="${GO_TOOLCHAIN_ROOT:-/home/heke/rcore/go-toolchain}"
export GOROOT="$TC/go"
export PATH="$TC/go/bin:$TC/bin:$TC/gopath/bin:/usr/bin:/bin"
export GOTOOLCHAIN=local
export GOPATH="$TC/gopath"
export GOMODCACHE="$TC/gomodcache"
export GOSUMDB=off
export GOFLAGS=-mod=mod
export GOCACHE="$TC/gocache"

# ---- OFFLINE module resolution decision (documented) ----
# `go mod tidy` on a freshly-generated go-zero/grpc project has NO require lines
# yet, so it must RESOLVE which module provides each bare import. With GOPROXY=off
# that resolution is disabled outright ("module lookup disabled by GOPROXY=off"),
# even though the modules are physically present in $GOMODCACHE. So GOPROXY=off
# alone makes tidy FAIL (recorded, never hangs — acceptable).
# BETTER (and what we do): the module cache also keeps the download index under
# $GOMODCACHE/cache/download. Pointing GOPROXY at that directory via file://
# gives `go mod tidy` a fully OFFLINE, deterministic proxy: it resolves + pins
# exactly the cached versions, no network. So:
#   * default GOPROXY=off for the whole script (build/codegen never need network);
#   * for the two E2E `go mod tidy` steps ONLY, use GOCTL_TIDY_PROXY, which
#     defaults to the local file:// cache. If that dir is missing, fall back to
#     the configured network GOPROXY (still hard-timeout-bounded). Either way the
#     script COMPLETES; an un-resolvable tidy records rc=false and moves on.
export GOPROXY="${GOPROXY:-off}"
_LOCAL_DL="$GOMODCACHE/cache/download"
if [ -d "$_LOCAL_DL" ]; then
  GOCTL_TIDY_PROXY="${GOCTL_TIDY_PROXY:-file://$_LOCAL_DL}"
else
  # No local cache index — fall back to a network proxy for tidy only.
  GOCTL_TIDY_PROXY="${GOCTL_TIDY_PROXY:-https://goproxy.cn,direct}"
fi

BASE="$(mktemp -d)"
export HOME="$BASE/home"; mkdir -p "$HOME"
export GOCTL_HOME="$HOME/.goctl"
trap 'rm -rf "$BASE"' EXIT

# ---- timeout guards ----
GOCTL_TIMEOUT="${GOCTL_TIMEOUT:-180}"
GOCTL_E2E_TIMEOUT="${GOCTL_E2E_TIMEOUT:-300}"
if command -v timeout >/dev/null 2>&1; then HAVE_TIMEOUT=1; else HAVE_TIMEOUT=0; fi
# run CMD...     -> guarded by GOCTL_TIMEOUT, stdin from /dev/null. rc in $?.
run() {
  if [ "$HAVE_TIMEOUT" -eq 1 ]; then timeout "$GOCTL_TIMEOUT" "$@" </dev/null; else "$@" </dev/null; fi
}
# run_e2e CMD... -> guarded by the longer GOCTL_E2E_TIMEOUT (slow build/tidy).
run_e2e() {
  if [ "$HAVE_TIMEOUT" -eq 1 ]; then timeout "$GOCTL_E2E_TIMEOUT" "$@" </dev/null; else "$@" </dev/null; fi
}
# run_out CMD... -> echo combined stdout+stderr (guarded). rc in $?.
run_out() {
  if [ "$HAVE_TIMEOUT" -eq 1 ]; then timeout "$GOCTL_TIMEOUT" "$@" </dev/null 2>&1; else "$@" </dev/null 2>&1; fi
}
# reap PID -> kill a background server and reap it WITHOUT ever blocking:
# SIGTERM, short grace, SIGKILL, then a timeout-bounded wait so the shell's
# own `wait` can never hang (the reap itself is capped at ~3s).
reap() {
  _p=$1
  kill "$_p" 2>/dev/null
  if [ "$HAVE_TIMEOUT" -eq 1 ]; then
    timeout 3 sh -c 'wait '"$_p" 2>/dev/null
  fi
  kill -9 "$_p" 2>/dev/null
  wait "$_p" 2>/dev/null
}

N=0
ok() { N=$((N+1)); printf 'ok: %s = %s\n' "$1" "$2"; }
b()  { if [ "$1" = "0" ]; then echo true; else echo false; fi; }
ex() { [ -e "$1" ] && echo true || echo false; }
strip() { sed 's/\x1b\[[0-9;]*m//g'; }

run goctl template init --home "$GOCTL_HOME" >/dev/null 2>&1

# ===== ROOT =====
V="$(run_out goctl --version | strip | tr -d '\n')"; ok root.--version.rc "$(b $?)"
[ "$V" = "goctl version 1.10.1 linux/amd64" ] && ok root.--version.exact true || ok root.--version.exact false
ok root.--version.string "$V"
[ "$(run_out goctl -v | strip | tr -d '\n')" = "$V" ] && ok root.-v.alias true || ok root.-v.alias false
H="$(run_out goctl --help | strip)"; ok root.--help.rc "$(b $?)"
ALL=true; for c in api bug completion config docker env gateway help kube migrate model quickstart rpc template upgrade; do echo "$H" | grep -qE "^[[:space:]]+$c[[:space:]]" || ALL=false; done
ok root.--help.lists-all-15 "$ALL"

# ===== api go PRIMARY E2E =====
AG="$BASE/apigo"; mkdir -p "$AG"
cat > "$AG/x.api" <<'API'
syntax = "v1"

type PingResp {
	Msg string `json:"msg"`
}

service x-api {
	@handler PingHandler
	get /ping returns (PingResp)
}
API
run goctl api go --api "$AG/x.api" --dir "$AG/out" --style gozero >/dev/null 2>&1; ok api-go.codegen.rc "$(b $?)"
ok api-go.tree "$( [ -f "$AG/out/x.go" ] && [ -f "$AG/out/internal/logic/pinglogic.go" ] && echo true || echo false )"
python3 - "$AG/out" <<'PY'
import sys,re
o=sys.argv[1]
p=o+"/internal/logic/pinglogic.go"; s=open(p).read()
s=re.sub(r"(?s)\t// todo:.*?\n\n\treturn[^\n]*\n", '\treturn &types.PingResp{Msg: "pong"}, nil\n', s); open(p,"w").write(s)
y=o+"/etc/x-api.yaml"; ys=open(y).read().replace("Host: 0.0.0.0","Host: 127.0.0.1").replace("Port: 8888","Port: 18888"); open(y,"w").write(ys)
PY
# tidy: use the offline file:// cache proxy (or network fallback), hard-bounded.
( cd "$AG/out" && GOPROXY="$GOCTL_TIDY_PROXY" run_e2e go mod tidy >/dev/null 2>&1 ); ok api-go.tidy.rc "$(b $?)"
( cd "$AG/out" && run_e2e go build ./... >/dev/null 2>&1 ); ok api-go.build.rc "$(b $?)"
( cd "$AG/out" && run_e2e go build -o "$AG/xserver" . >/dev/null 2>&1 ); ok api-go.bin.rc "$(b $?)"
if [ -x "$AG/xserver" ]; then
  run "$AG/xserver" -f "$AG/out/etc/x-api.yaml" >/dev/null 2>&1 &
  SRV=$!
  BODY=""; for i in $(seq 1 60); do BODY="$(run_out curl -s http://127.0.0.1:18888/ping)"; [ -n "$BODY" ] && break; sleep 0.25; done
  CODE="$(run_out curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18888/ping)"
  reap "$SRV"
  ok api-go.http.status "$CODE"
  ok api-go.http.body "$BODY"
  [ "$BODY" = '{"msg":"pong"}' ] && ok api-go.http.body-is-pong true || ok api-go.http.body-is-pong false
else
  ok api-go.http.E2E "SKIP: server binary not built (build/tidy failed or timed out)"
fi

# ===== api go regen with extra flags (--test generates *_test.go; --type-group) =====
run goctl api go --api "$AG/x.api" --dir "$AG/outt" --style gozero --test >/dev/null 2>&1; ok api-go.test-flag.rc "$(b $?)"
ok api-go.test-flag.file "$( find "$AG/outt" -name '*_test.go' 2>/dev/null | head -1 | grep -q . && echo true || echo false )"
run goctl api go --api "$AG/x.api" --dir "$AG/outtg" --style gozero --type-group >/dev/null 2>&1; ok api-go.type-group.rc "$(b $?)"

# ===== api generators =====
run goctl api validate --api "$AG/x.api" >/dev/null 2>&1; ok api.validate.valid "$(b $?)"
printf 'syntax = "v1"\nservice {' > "$BASE/bad.api"; run goctl api validate --api "$BASE/bad.api" >/dev/null 2>&1; [ $? -ne 0 ] && ok api.validate.invalid true || ok api.validate.invalid false
SW="$BASE/sw"; mkdir -p "$SW"; run goctl api swagger --api "$AG/x.api" --dir "$SW" --filename s >/dev/null 2>&1; ok api.swagger.json "$(ex "$SW/s.json")"
run goctl api swagger --api "$AG/x.api" --dir "$SW" --filename sy --yaml >/dev/null 2>&1; ok api.swagger.yaml "$(ex "$SW/sy.yaml")"
TS="$BASE/ts"; mkdir -p "$TS"; run goctl api ts --api "$AG/x.api" --dir "$TS" --caller webapi >/dev/null 2>&1; ok api.ts.rc "$(b $?)"
DT="$BASE/dart"; mkdir -p "$DT"; run goctl api dart --api "$AG/x.api" --dir "$DT" >/dev/null 2>&1; ok api.dart.rc "$(b $?)"
KT="$BASE/kt"; mkdir -p "$KT"; run goctl api kt --api "$AG/x.api" --dir "$KT" --pkg com.x >/dev/null 2>&1; ok api.kt.BaseApi "$(ex "$KT/BaseApi.kt")"
DC="$BASE/doc"; DCs="$BASE/docsrc"; mkdir -p "$DC" "$DCs"; cp "$AG/x.api" "$DCs/"; run goctl api doc --dir "$DCs" --o "$DC" >/dev/null 2>&1; ok api.doc.rc "$(b $?)"
# api format: format an .api file in place via --dir; assert rc true.
AF="$BASE/apifmt"; mkdir -p "$AF"
printf 'syntax = "v1"\ntype Foo {Bar string `json:"bar"`}\nservice f-api {\n@handler FooHandler\nget /foo returns (Foo)\n}\n' > "$AF/f.api"
( cd "$AF" && run goctl api format --dir "$AF" >/dev/null 2>&1 ); ok api.format.rc "$(b $?)"
# api new: scaffold a fresh api project (hermetic template under GOCTL_HOME).
ANEW="$BASE/apinew"; mkdir -p "$ANEW"
( cd "$ANEW" && run goctl api new greet --home "$GOCTL_HOME" >/dev/null 2>&1 ); ok api.new.rc "$(b $?)"
ok api.new.api "$(ex "$ANEW/greet/greet.api")"
ok api.new.main "$(ex "$ANEW/greet/greet.go")"
# api plugin --help: plugin execution needs an external plugin binary -> help only.
APH="$(run_out goctl api plugin --help)"; ok api.plugin.--help.rc "$(b $?)"
echo "$APH" | grep -qi "plugin" && ok api.plugin.--help.mentions-plugin true || ok api.plugin.--help.mentions-plugin false
ok api.plugin.run "SKIP: needs external plugin binary (-p plugin.file)"
# --remote / --branch global template flags: surfaced in api go --help (no network hit).
AGH="$(run_out goctl api go --help)"; ok api-go.--help.rc "$(b $?)"
echo "$AGH" | grep -q -- "--remote" && ok api-go.--help.has-remote true || ok api-go.--help.has-remote false
echo "$AGH" | grep -q -- "--branch" && ok api-go.--help.has-branch true || ok api-go.--help.has-branch false
echo "$AGH" | grep -q -- "--home"   && ok api-go.--help.has-home   true || ok api-go.--help.has-home   false

# ===== rpc protoc PRIMARY E2E =====
if command -v protoc >/dev/null 2>&1; then
  RP="$BASE/rpcproj"; mkdir -p "$RP"
  cat > "$RP/x.proto" <<'PROTO'
syntax = "proto3";
package x;
option go_package = "./pb";
message PingReq { string name = 1; }
message PingResp { string msg = 1; }
service X { rpc Ping(PingReq) returns(PingResp); }
PROTO
  ( cd "$RP" && run goctl rpc protoc x.proto --go_out=. --go-grpc_out=. --zrpc_out=. --style gozero >/dev/null 2>&1 ); ok rpc-protoc.codegen.rc "$(b $?)"
  ok rpc-protoc.pb "$( [ -f "$RP/pb/x.pb.go" ] && [ -f "$RP/pb/x_grpc.pb.go" ] && echo true || echo false )"
  python3 - "$RP" <<'PY'
import sys,re
r=sys.argv[1]; p=r+"/internal/logic/pinglogic.go"; s=open(p).read()
s=re.sub(r"(?s)\t// todo:.*?\n\n\treturn &pb\.PingResp\{\}, nil\n", '\treturn &pb.PingResp{Msg: "pong:" + in.Name}, nil\n', s); open(p,"w").write(s)
open(r+"/etc/x.yaml","w").write("Name: x.rpc\nListenOn: 127.0.0.1:18901\n")
import os
os.makedirs(r+"/cmd/pingcli",exist_ok=True)
open(r+"/cmd/pingcli/main.go","w").write('''package main
import ("context";"fmt";"os";"time";"rpcproj/pb";"google.golang.org/grpc";"google.golang.org/grpc/credentials/insecure")
func main(){c,e:=grpc.NewClient("127.0.0.1:18901",grpc.WithTransportCredentials(insecure.NewCredentials()));if e!=nil{fmt.Println(e);os.Exit(1)};defer c.Close();cli:=pb.NewXClient(c);ctx,cl:=context.WithTimeout(context.Background(),5*time.Second);defer cl();r,e:=cli.Ping(ctx,&pb.PingReq{Name:"hi"});if e!=nil{fmt.Println(e);os.Exit(1)};fmt.Print("RESP="+r.Msg)}''')
PY
  ( cd "$RP" && GOPROXY="$GOCTL_TIDY_PROXY" run_e2e go mod tidy >/dev/null 2>&1 ); ok rpc-protoc.tidy.rc "$(b $?)"
  ( cd "$RP" && run_e2e go build ./... >/dev/null 2>&1 ); ok rpc-protoc.build.rc "$(b $?)"
  ( cd "$RP" && run_e2e go build -o "$BASE/rpcsrv" . >/dev/null 2>&1 )
  ( cd "$RP" && run_e2e go build -o "$BASE/rpccli" ./cmd/pingcli >/dev/null 2>&1 )
  if [ -x "$BASE/rpcsrv" ] && [ -x "$BASE/rpccli" ]; then
    ( cd "$RP" && run "$BASE/rpcsrv" -f etc/x.yaml >/dev/null 2>&1 ) &
    SRV=$!
    RESP=""; for i in $(seq 1 60); do RESP="$(run_out "$BASE/rpccli")"; [ "${RESP#RESP=}" != "$RESP" ] && break; sleep 0.25; done
    reap "$SRV"
    ok rpc-protoc.grpc.resp "$RESP"
    [ "$RESP" = "RESP=pong:hi" ] && ok rpc-protoc.grpc.echoed true || ok rpc-protoc.grpc.echoed false
  else
    ok rpc-protoc.grpc.E2E "SKIP: server/client binary not built (build/tidy failed or timed out)"
  fi
else
  ok rpc-protoc.E2E "SKIP: protoc compiler not installed"
fi

# ===== rpc new / rpc template (hermetic, no protoc/network needed) =====
RN="$BASE/rpcnew"; mkdir -p "$RN"
( cd "$RN" && run goctl rpc new gr --home "$GOCTL_HOME" >/dev/null 2>&1 ); ok rpc.new.rc "$(b $?)"
ok rpc.new.proto "$(ex "$RN/gr/gr.proto")"
ok rpc.new.go    "$(ex "$RN/gr/gr.go")"
RT="$BASE/rpctpl"; mkdir -p "$RT"
( cd "$RT" && run goctl rpc template --home "$GOCTL_HOME" --o "$RT/sample.proto" >/dev/null 2>&1 ); ok rpc.template.rc "$(b $?)"
ok rpc.template.proto "$(ex "$RT/sample.proto")"

# ===== model (hermetic) =====
MD="$BASE/model"; mkdir -p "$MD"
cat > "$MD/u.sql" <<'SQL'
CREATE TABLE `user` (`id` bigint NOT NULL AUTO_INCREMENT, `name` varchar(255) NOT NULL DEFAULT '', PRIMARY KEY (`id`)) ENGINE=InnoDB;
SQL
run goctl model mysql ddl -s "$MD/u.sql" -d "$MD/m" >/dev/null 2>&1; ok model.mysql.ddl.rc "$(b $?)"; ok model.mysql.ddl.file "$(ex "$MD/m/usermodel.go")"
run goctl model mysql ddl -s "$MD/u.sql" -d "$MD/mc" -c >/dev/null 2>&1; ok model.mysql.ddl.cache.rc "$(b $?)"
# extend model mysql ddl: --idea + --prefix + --strict (all on the cache path).
run goctl model mysql ddl -s "$MD/u.sql" -d "$MD/ms" -c --idea --prefix myp --strict >/dev/null 2>&1; ok model.mysql.ddl.idea-prefix-strict.rc "$(b $?)"; ok model.mysql.ddl.idea-prefix-strict.file "$(ex "$MD/ms/usermodel.go")"
run goctl model mongo -t User -d "$MD/mg" >/dev/null 2>&1; ok model.mongo.rc "$(b $?)"; ok model.mongo.file "$(ex "$MD/mg/usermodel.go")"
# model mongo: -e/--easy + multiple -t/--type.
run goctl model mongo -t User -t Order -d "$MD/mge" -e >/dev/null 2>&1; ok model.mongo.easy-type.rc "$(b $?)"; ok model.mongo.easy-type.file "$(ex "$MD/mge/ordermodel.go")"
ok model.mysql.datasource "SKIP: requires reachable MySQL (no loopback DB)"
ok model.pg.datasource "SKIP: requires reachable PostgreSQL (no loopback DB)"
ok model.mongo.datasource "SKIP: requires reachable MongoDB (no loopback DB)"

# ===== docker / kube / gateway / completion / env / config / template =====
DK="$BASE/dk"; mkdir -p "$DK"; printf 'module dk\n\ngo 1.26.3\n' > "$DK/go.mod"; printf 'package main\nfunc main(){}\n' > "$DK/main.go"
( cd "$DK" && run goctl docker --go main.go --port 8888 --exe myapp >/dev/null 2>&1 ); ok docker.rc "$(b $?)"; ok docker.Dockerfile "$(ex "$DK/Dockerfile")"
run goctl kube deploy --name a --namespace n --image i:1 --port 8080 --o "$BASE/k.yaml" >/dev/null 2>&1; ok kube.deploy.rc "$(b $?)"; ok kube.deploy.yaml "$(ex "$BASE/k.yaml")"
GW="$BASE/gw"; mkdir -p "$GW"; run goctl gateway --dir "$GW" >/dev/null 2>&1; ok gateway.rc "$(b $?)"; ok gateway.main "$(ex "$GW/main.go")"
for sh in bash zsh fish powershell; do run goctl completion $sh >/dev/null 2>&1; ok completion.$sh.rc "$(b $?)"; done
run goctl env check >/dev/null 2>&1; ok env.check.rc "$(b $?)"
run goctl env >/dev/null 2>&1; ok env.print.rc "$(b $?)"
ok env.install "SKIP: installs protoc / protoc-gen-* (modifies host)"
CF="$BASE/cfg"; mkdir -p "$CF"; ( cd "$CF" && run goctl config init >/dev/null 2>&1 ); ok config.init.rc "$(b $?)"; ok config.init.yaml "$(ex "$CF/goctl.yaml")"
( cd "$CF" && run goctl config clean >/dev/null 2>&1 ); ok config.clean.rc "$(b $?)"
TH="$BASE/th"; run goctl template init --home "$TH" >/dev/null 2>&1; ok template.init.rc "$(b $?)"
run goctl template clean --home "$TH" >/dev/null 2>&1; ok template.clean.rc "$(b $?)"
# template revert / update: both contact the remote template repo when RUN, so
# exercise --help only (rc true + correct synopsis surface).
TRH="$(run_out goctl template revert --help)"; ok template.revert.--help.rc "$(b $?)"
echo "$TRH" | grep -qi "revert" && ok template.revert.--help.mentions-revert true || ok template.revert.--help.mentions-revert false
TUH="$(run_out goctl template update --help)"; ok template.update.--help.rc "$(b $?)"
echo "$TUH" | grep -qi "update" && ok template.update.--help.mentions-update true || ok template.update.--help.mentions-update false

# ===== maintenance (help only; runs SKIPped) =====
run goctl migrate --help >/dev/null 2>&1; ok migrate.--help.rc "$(b $?)"; ok migrate.run "SKIP: rewrites imports (destructive)"
run goctl quickstart --help >/dev/null 2>&1; ok quickstart.--help.rc "$(b $?)"; ok quickstart.run "SKIP: pulls remote template + long build"
run goctl bug --help >/dev/null 2>&1; ok bug.--help.rc "$(b $?)"; ok bug.run "SKIP: opens browser / interactive"
run goctl upgrade --help >/dev/null 2>&1; ok upgrade.--help.rc "$(b $?)"; ok upgrade.run "SKIP: network; replaces pinned binary"

printf 'GOCTL_COUNT=%d\n' "$N"
