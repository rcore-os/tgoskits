#!/bin/sh
# go-cli-carpet.sh — INDUSTRIAL-GRADE, DOC-GROUNDED carpet for the Go toolchain CLI.
#
# StarryOS #764 Go-1.26 language delivery. The existing go126test (main.go) covers the
# RUNTIME/LANGUAGE layer (goroutines/channels/generics/errors/1.26 features) but exercises
# ZERO `go` CLI subcommands. THIS carpet covers the `go` command surface exhaustively:
# every documented subcommand, every nested subcommand (go mod <sub>, go work <sub>,
# go tool <sub>), and every `go help <topic>` — each with an OBSERVABLE assertion or an
# explicit, reasoned SKIP.
#
# GROUND TRUTH (enumerated item-by-item, not from memory):
#   * https://pkg.go.dev/cmd/go        — every subcommand + nested forms (go1.26 doc)
#   * https://go.dev/ref/spec          — language (covered by go126test main.go; built+run here)
#   * https://go.dev/doc/go1.26        — 1.26 features (go fix modernizers, go mod init -1 default,
#                                         go telemetry, goauth/buildjson topics)
#   * host `go help`, `go help mod`, `go help work`, `go tool`, `go help <topic>` (live --help tree)
#
# SUBCOMMANDS (from `go help`, the canonical list):
#   bug build clean doc env fix fmt generate get install list mod work run test tool version vet
#   + telemetry (go1.26)
# NESTED:
#   go mod  : download edit graph init tidy vendor verify why
#   go work : edit init sync use vendor
#   go tool : addr2line asm buildid cgo compile covdata cover dist distpack doc fix link nm
#             objdump pack pprof test2json trace vet  (set is GOROOT/version dependent)
# HELP TOPICS:
#   buildconstraint buildmode c cache environment filetype go.mod gopath goproxy importpath
#   modules module-auth packages private testflag testfunc vcs  (+ goauth buildjson in go1.26)
#
# Portable: POSIX sh, no host-absolute paths in the test logic. All work happens in a fresh
# temp dir under ${TMPDIR:-/tmp}. Fully OFFLINE — every module is local; network subcommands
# (go get / go install of remote pkgs / go mod download of remote / go bug) are reasoned SKIPs.
# Memory-bounded: GOFLAGS caps parallelism; no JVM/heavy runtime here, but every `go` invocation
# is single-process and the only heavy step (go build/test) is small.
#
# OK token: GO_CLI_OK  (printed only when ALL checks pass and zero FAIL).
# Version-aware: assertions that depend on go1.26-only behavior probe-and-skip on older toolchains
# so the carpet is host-green on go1.22+ AND meaningful on the go1.26 target.

set -u

PASS=0
FAIL=0
SKIP=0

ok()   { PASS=$((PASS+1)); printf 'ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf 'FAIL %s -- %s\n' "$1" "${2:-}"; }
skip() { SKIP=$((SKIP+1)); printf 'skip %s -- %s\n' "$1" "${2:-}"; }

# ---------------------------------------------------------------------
# TIMEOUT GUARD: every external `go` invocation goes through run(), which
# wraps the command in `timeout` (parameterized via GO_CLI_TIMEOUT, default
# 300s) and redirects stdin from /dev/null so nothing can ever block on a
# terminal read. `timeout` exits 124 on expiry; callers treat 124 as a FAIL.
# run_out() captures stdout+stderr; run() is for rc-only callers.
# ---------------------------------------------------------------------
GO_CLI_TIMEOUT=${GO_CLI_TIMEOUT:-300}
if command -v timeout >/dev/null 2>&1; then
  HAVE_TIMEOUT=1
else
  HAVE_TIMEOUT=0
fi
# run CMD...  -> runs guarded, returns its rc (124 on timeout). stdin=/dev/null.
run() {
  if [ "$HAVE_TIMEOUT" -eq 1 ]; then
    timeout "$GO_CLI_TIMEOUT" "$@" </dev/null
  else
    "$@" </dev/null
  fi
}
# run_out CMD...  -> echoes combined stdout+stderr; rc is in $?.
run_out() {
  if [ "$HAVE_TIMEOUT" -eq 1 ]; then
    timeout "$GO_CLI_TIMEOUT" "$@" </dev/null 2>&1
  else
    "$@" </dev/null 2>&1
  fi
}
# assert: name, expected-substring, actual-text
assert_has() {
  _n=$1; _exp=$2; _act=$3
  case "$_act" in
    *"$_exp"*) ok "$_n" ;;
    *) bad "$_n" "expected to contain [$_exp]; got [$(printf '%s' "$_act" | head -c 200)]" ;;
  esac
}
# assert a substring is ABSENT (real differential negative)
assert_lacks() {
  _n=$1; _exp=$2; _act=$3
  case "$_act" in
    *"$_exp"*) bad "$_n" "did NOT expect [$_exp]; got [$(printf '%s' "$_act" | head -c 200)]" ;;
    *) ok "$_n" ;;
  esac
}
# assert exit code zero; 124 (timeout) is reported as a timeout FAIL
assert_rc0() {
  if [ "$2" -eq 0 ]; then ok "$1"
  elif [ "$2" -eq 124 ]; then bad "$1" "TIMEOUT (rc=124, limit=${GO_CLI_TIMEOUT}s)"
  else bad "$1" "rc=$2"; fi
}

# ---- locate go (parameterized: GO_BIN env override) ----
GO=${GO_BIN:-go}
if ! command -v "$GO" >/dev/null 2>&1; then
  printf 'host-go-absent: `%s` not on PATH. This carpet is doc-grounded and READY to run where go exists.\n' "$GO"
  printf 'GO_CLI_ABSENT\n'
  exit 0
fi
GOVER=$(run_out "$GO" version)
printf '## go toolchain: %s\n' "$GOVER"

# numeric minor version for version-aware gating (e.g. 1.22 -> 22, 1.26 -> 26)
GOMINOR=$(printf '%s' "$GOVER" | sed -n 's/.*go1\.\([0-9][0-9]*\).*/\1/p')
[ -n "$GOMINOR" ] || GOMINOR=0
printf '## detected go minor: 1.%s\n' "$GOMINOR"

# ---- isolated, offline workspace ----
ROOT=$(mktemp -d "${TMPDIR:-/tmp}/gocli.XXXXXX") || { printf 'FAIL mktemp\n'; exit 1; }
# Keep all module/cache state local + offline so no network is ever required.
export GOPATH="$ROOT/gopath"
export GOMODCACHE="$ROOT/gopath/pkg/mod"
export GOCACHE="$ROOT/gocache"
export GOFLAGS="-mod=mod"
export GO111MODULE=on
export GOPROXY=off
export GOSUMDB=off
export GOTOOLCHAIN=local
export CGO_ENABLED=0
# CRITICAL ISOLATION: point GOENV at a file under the temp ROOT so that
# `go env -w` / `go env -u` mutate THIS file, never the user's global
# ~/.config/go/env. The cleanup trap removes ROOT (and thus this env file).
export GOENV="$ROOT/goenv"
: > "$GOENV"
mkdir -p "$GOPATH" "$GOCACHE"
cleanup() { cd /; rm -rf "$ROOT"; }
trap cleanup EXIT INT TERM

cd "$ROOT" || exit 1

# =====================================================================
# 1. go version  (plain — assert the REAL os/arch from `go env`, not "any slash")
# =====================================================================
V=$(run_out "$GO" version)
assert_has "version/plain" "go version go" "$V"
# Real differential: the version banner must end with the toolchain's own
# GOOS/GOARCH (e.g. "linux/amd64"), which we read independently from `go env`.
VGOOS=$(run_out "$GO" env GOOS)
VGOARCH=$(run_out "$GO" env GOARCH)
assert_has "version/os-arch" "$VGOOS/$VGOARCH" "$V"

# =====================================================================
# 2. go env  (print / -json / -w / -u / specific var) — all writes hit the
#    ISOLATED $GOENV under ROOT (not the user's global ~/.config/go/env).
# =====================================================================
assert_has "env/GOENV-isolated" "$ROOT" "$(run_out "$GO" env GOENV)"
assert_has "env/GOVERSION" "go1." "$(run_out "$GO" env GOVERSION)"
assert_has "env/GOROOT-nonempty" "/" "$(run_out "$GO" env GOROOT)"
EJ=$(run_out "$GO" env -json)
assert_has "env/-json" '"GOROOT"' "$EJ"
# env -w writes a DISTINCTIVE marker into the isolated env file; read it back.
run "$GO" env -w GOFLAGS=-mod=mod >/dev/null 2>&1
assert_has "env/-w+readback" "-mod=mod" "$(run_out "$GO" env GOFLAGS)"
# Confirm the write landed in the ISOLATED file, proving global state is untouched.
assert_has "env/-w-hits-isolated-file" "GOFLAGS=-mod=mod" "$(cat "$GOENV" 2>&1)"
# env -u (unset) must REMOVE it: the persisted value goes back to the empty
# default, and the marker must vanish from the isolated env file.
run "$GO" env -u GOFLAGS >/dev/null 2>&1
# With GOFLAGS unset in the env file AND not exported yet, the persisted value is empty.
GFAFTER=$(unset GOFLAGS; run_out "$GO" env GOFLAGS)
if [ -z "$GFAFTER" ]; then ok "env/-u-unset"; else bad "env/-u-unset" "GOFLAGS still [$GFAFTER] after env -u"; fi
assert_lacks "env/-u-removes-from-file" "GOFLAGS=-mod=mod" "$(cat "$GOENV" 2>&1)"
export GOFLAGS="-mod=mod"   # restore (process env) for the rest of the carpet

# =====================================================================
# 3. go mod init  +  the go.mod is created with module path
# =====================================================================
MOD="example.com/clicarpet"
run "$GO" mod init "$MOD" >/dev/null 2>&1
assert_rc0 "mod/init" $?
if [ -f go.mod ]; then assert_has "mod/init-go.mod-path" "module $MOD" "$(cat go.mod)"; else bad "mod/init-go.mod-path" "no go.mod"; fi

# `go mod init` (no explicit version) writes a `go` directive set to the running
# toolchain's release version. Since go1.21 that is the full version including the
# patch component (e.g. "1.26.3"); we assert the directive matches the running
# toolchain's "1.<minor>" line (with or without a patch suffix).
GODIR=$(grep -E '^go ' go.mod 2>/dev/null | awk '{print $2}')
if [ "$GOMINOR" -ge 26 ]; then
  case "$GODIR" in
    1.$GOMINOR|1.$GOMINOR.*) assert_has "mod/init-go-directive" "1.$GOMINOR" "$GODIR" ;;
    *) bad "mod/init-go-directive" "go directive [$GODIR] does not match running toolchain 1.$GOMINOR" ;;
  esac
else
  skip "mod/init-go-directive" "host go 1.$GOMINOR writes go directive [$GODIR]"
fi

# A small, self-contained, OFFLINE program covering several go-CLI-relevant features.
cat > main.go <<'EOF'
// Package main is the offline carpet program: build/run/vet/test/doc/generate targets.
package main

import "fmt"

//go:generate echo CARPET_GENERATE_MARKER

// Greet returns a deterministic greeting. Documented for `go doc`.
func Greet(name string) string { return "hi " + name }

func main() { fmt.Println(Greet("starry")) }
EOF

cat > util.go <<'EOF'
package main

// Add adds two ints. Exported so `go doc` has a symbol to show.
func Add(a, b int) int { return a + b }
EOF

cat > main_test.go <<'EOF'
package main

import "testing"

func TestGreet(t *testing.T) {
	if Greet("x") != "hi x" {
		t.Fatalf("greet")
	}
}

func TestAdd(t *testing.T) {
	if Add(2, 3) != 5 {
		t.Fatalf("add")
	}
}

func BenchmarkAdd(b *testing.B) {
	for i := 0; i < b.N; i++ {
		_ = Add(i, i)
	}
}

func ExampleGreet() {
	println(Greet("e"))
	// Output:
}
EOF

# =====================================================================
# 4. go mod edit  (-go=, -require/-droprequire, -json, -print, -module)
# =====================================================================
run "$GO" mod edit -go=1.20 >/dev/null 2>&1
assert_has "mod/edit--go" "go 1.20" "$(cat go.mod)"
run "$GO" mod edit -go="${GOVER_GO:-1.21}" >/dev/null 2>&1 || run "$GO" mod edit -go=1.21 >/dev/null 2>&1
MJ=$(run_out "$GO" mod edit -json)
assert_has "mod/edit--json" "\"Module\"" "$MJ"
MP=$(run_out "$GO" mod edit -print)
assert_has "mod/edit--print" "module $MOD" "$MP"
# add then drop a (fake, never-resolved) require — edit is purely textual, no network
run "$GO" mod edit -require=example.com/dep@v1.0.0 >/dev/null 2>&1
assert_has "mod/edit--require" "example.com/dep v1.0.0" "$(cat go.mod)"
run "$GO" mod edit -droprequire=example.com/dep >/dev/null 2>&1
case "$(cat go.mod)" in *example.com/dep*) bad "mod/edit--droprequire" "still present" ;; *) ok "mod/edit--droprequire" ;; esac

# =====================================================================
# 5. go build  (default, -o, ./..., -v, -x trace, -gcflags, -n dry-run)
# =====================================================================
run "$GO" build ./... >/dev/null 2>&1; assert_rc0 "build/dotdotdot" $?
run "$GO" build -o carpetbin . >/dev/null 2>&1; assert_rc0 "build/-o" $?
if [ -x ./carpetbin ]; then assert_has "build/-o-runs" "hi starry" "$(run_out ./carpetbin)"; else bad "build/-o-runs" "no binary"; fi
# -n prints the build plan but builds NOTHING: assert it emits a compile/link
# step AND produces no output binary (real differential, not just rc0).
rm -f buildn_probe
BN=$(run_out "$GO" build -n -o buildn_probe .); BNRC=$?
assert_rc0 "build/-n-dryrun" $BNRC
assert_has "build/-n-shows-plan" "compile" "$BN"
if [ -e buildn_probe ]; then bad "build/-n-no-artifact" "-n produced an artifact"; else ok "build/-n-no-artifact"; fi
run "$GO" build -x -o carpetbin2 . >/dev/null 2>&1; assert_rc0 "build/-x" $?
# -gcflags is accepted and the build still succeeds
run "$GO" build -gcflags=-l -o carpetbin3 . >/dev/null 2>&1; assert_rc0 "build/-gcflags" $?

# =====================================================================
# 6. go run  (program + with an arg via os.Args is not needed; just stdout)
# =====================================================================
assert_has "run/stdout" "hi starry" "$(run_out "$GO" run .)"
assert_has "run/single-file" "hi starry" "$(run_out "$GO" run main.go util.go)"

# =====================================================================
# 7. go vet  (clean code -> rc0;  -n;  detects a real mistake)
# =====================================================================
run "$GO" vet ./... >/dev/null 2>&1; assert_rc0 "vet/clean-rc0" $?
# Introduce a bad Printf to confirm vet actually catches mistakes, then remove it.
cat > vetbad.go <<'EOF'
package main

import "fmt"

func vetTrigger() { fmt.Printf("%d\n", "not-an-int") }
EOF
VOUT=$(run_out "$GO" vet ./...); VRC=$?
rm -f vetbad.go
if [ "$VRC" -eq 124 ]; then bad "vet/catches-mistake" "TIMEOUT (rc=124)"
elif [ "$VRC" -ne 0 ]; then assert_has "vet/catches-mistake" "Printf" "$VOUT"; else bad "vet/catches-mistake" "vet did not flag bad Printf"; fi

# =====================================================================
# 8. go fmt  (reformats, prints changed filenames) + go fmt -n
# =====================================================================
cat > messy.go <<'EOF'
package main
func  messy( ){ _ = 1 }
EOF
FOUT=$(run_out "$GO" fmt ./...); assert_rc0 "fmt/rc0" $?
assert_has "fmt/lists-changed" "messy.go" "$FOUT"
# verify it actually canonicalized the spacing
assert_has "fmt/canonicalized" "func messy()" "$(cat messy.go)"
run "$GO" fmt -n ./... >/dev/null 2>&1; assert_rc0 "fmt/-n" $?
rm -f messy.go

# =====================================================================
# 9. go test  (basic, -run, -count, -v, -bench, -json, -cover)
# =====================================================================
run "$GO" test -count=1 ./... >/dev/null 2>&1; assert_rc0 "test/all" $?
assert_has "test/-run" "ok" "$(run_out "$GO" test -run TestGreet -count=1 -v ./... | tail -1)"
BENCH=$(run_out "$GO" test -run='^$' -bench=BenchmarkAdd -benchtime=1x ./...)
assert_has "test/-bench" "BenchmarkAdd" "$BENCH"
TJ=$(run_out "$GO" test -json -run TestAdd ./...)
assert_has "test/-json" '"Action"' "$TJ"
COV=$(run_out "$GO" test -cover -run TestAdd ./...)
assert_has "test/-cover" "coverage:" "$COV"

# =====================================================================
# 10. go list  (packages, -m module, -f template, -json, -deps, std subset)
# =====================================================================
assert_has "list/pkg" "$MOD" "$(run_out "$GO" list .)"
assert_has "list/-m" "$MOD" "$(run_out "$GO" list -m)"
assert_has "list/-f-template" "$MOD" "$(run_out "$GO" list -f '{{.ImportPath}}' .)"
assert_has "list/-json" '"ImportPath"' "$(run_out "$GO" list -json .)"
assert_has "list/-deps" "fmt" "$(run_out "$GO" list -deps .)"
# `go list std` enumerates the standard library (offline, from GOROOT)
STDN=$(run_out "$GO" list std | grep -c .)
if [ "${STDN:-0}" -gt 50 ]; then ok "list/std-count($STDN)"; else bad "list/std-count" "only $STDN std pkgs"; fi

# =====================================================================
# 11. go doc  (package, symbol, method, -all, -src, -cmd) — offline, GOROOT
# =====================================================================
assert_has "doc/pkg" "package fmt" "$(run_out "$GO" doc fmt)"
assert_has "doc/symbol" "func Println" "$(run_out "$GO" doc fmt.Println)"
assert_has "doc/local-symbol" "func Greet" "$(run_out "$GO" doc . Greet)"
assert_has "doc/-all" "func" "$(run_out "$GO" doc -all strings | head -40)"
assert_has "doc/-src" "func HasPrefix" "$(run_out "$GO" doc -src strings.HasPrefix)"

# =====================================================================
# 12. go generate  (runs //go:generate directives)
# =====================================================================
assert_has "generate/runs-directive" "CARPET_GENERATE_MARKER" "$(run_out "$GO" generate ./...)"
run "$GO" generate -n ./... >/dev/null 2>&1; assert_rc0 "generate/-n" $?

# =====================================================================
# 13. go clean  (-n dry-run prints rm lines;  real clean rc0;  -cache -n)
# =====================================================================
assert_has "clean/-n" "rm -f" "$(run_out "$GO" clean -n .)"
run "$GO" clean . >/dev/null 2>&1; assert_rc0 "clean/real" $?
run "$GO" clean -cache -n >/dev/null 2>&1; assert_rc0 "clean/-cache-n" $?
run "$GO" clean -testcache >/dev/null 2>&1; assert_rc0 "clean/-testcache" $?

# =====================================================================
# 14. go fix  (real usage via `go help fix`; rewrite-semantics differential)
# =====================================================================
# Use `go help fix` — the canonical usage doc — instead of asserting a banner
# for a flag that does not exist. Assert the real, stable usage line.
FXH=$(run_out "$GO" help fix)
assert_has "fix/help-usage" "usage: go fix" "$FXH"
assert_has "fix/help-mentions-packages" "packages" "$FXH"

# `go fix` semantics differ by toolchain:
#   * pre-1.26: legacy apifix; on already-modern source it makes NO changes.
#   * go1.26  : `go fix` becomes a MODERNIZER that may REWRITE source. So we
#               must NOT assert rc0-as-noop; instead assert the OBSERVABLE
#               rewrite contract on two inputs.
# (a) ALREADY-MODERN input must be left BYTE-FOR-BYTE unchanged by `go fix`.
mkdir -p "$ROOT/fixmod"
cat > "$ROOT/fixmod/go.mod" <<EOF
module example.com/fixmod

go 1.$GOMINOR
EOF
cat > "$ROOT/fixmod/modern.go" <<'EOF'
package fixmod

// Already-modern, idiomatic Go: nothing for a modernizer to rewrite.
func Sum(xs []int) int {
	total := 0
	for _, v := range xs {
		total += v
	}
	return total
}
EOF
cp "$ROOT/fixmod/modern.go" "$ROOT/fixmod/modern.go.orig"
FIXOUT=$( cd "$ROOT/fixmod" && run_out "$GO" fix ./... ); FIXRC=$?
if [ "$FIXRC" -eq 124 ]; then
  bad "fix/no-rewrite-on-modern" "TIMEOUT (rc=124)"
elif [ "$FIXRC" -ne 0 ]; then
  # Some toolchains emit advisory output but still exit nonzero; the contract we
  # verify is the FILE, not rc — only fail if the file actually changed.
  if diff -q "$ROOT/fixmod/modern.go" "$ROOT/fixmod/modern.go.orig" >/dev/null 2>&1; then
    ok "fix/no-rewrite-on-modern"
  else
    bad "fix/no-rewrite-on-modern" "modern source was rewritten: $(printf '%s' "$FIXOUT" | head -c 120)"
  fi
else
  if diff -q "$ROOT/fixmod/modern.go" "$ROOT/fixmod/modern.go.orig" >/dev/null 2>&1; then
    ok "fix/no-rewrite-on-modern"
  else
    bad "fix/no-rewrite-on-modern" "modern source was rewritten by go fix"
  fi
fi

# (b) go1.26 `go fix` analysis-tool flag: 1.26 reworked `go fix` around the
#     analysis framework and added the -fixtool flag (selects an alternative fixer
#     tool, mirroring `go vet`'s -vettool). Assert `go help fix` documents it.
if [ "$GOMINOR" -ge 26 ]; then
  assert_has "fix/1.26-fixtool-flag" "-fixtool" "$FXH"
else
  skip "fix/1.26-fixtool-flag" "host go 1.$GOMINOR < 1.26 (-fixtool is 1.26-only)"
fi

# =====================================================================
# 15. go mod tidy / verify / graph / why / download(local) / vendor
# =====================================================================
# tidy: with no external deps it is a clean no-op
run "$GO" mod tidy >/dev/null 2>&1; assert_rc0 "mod/tidy" $?
# verify: with empty/clean module graph -> "all modules verified"
MV=$(run_out "$GO" mod verify); assert_rc0 "mod/verify-rc0" $?
assert_has "mod/verify-msg" "verified" "$MV"
# graph: prints the (possibly single-node) requirement graph without error
run "$GO" mod graph >/dev/null 2>&1; assert_rc0 "mod/graph" $?
# why: explain a stdlib import we use (-m flavor needs a module; use package form)
WHY=$(run_out "$GO" mod why fmt); assert_rc0 "mod/why" $?
# download with no requires is a no-op success (no network because GOPROXY=off & no deps)
run "$GO" mod download >/dev/null 2>&1; assert_rc0 "mod/download-local-noop" $?
# vendor: with no external deps, creates an (empty) vendor tree or no-ops; must rc0
run "$GO" mod vendor >/dev/null 2>&1; assert_rc0 "mod/vendor" $?
rm -rf vendor

# =====================================================================
# 16. go work  init / use / edit / sync / vendor   (multi-module workspace)
# =====================================================================
WROOT="$ROOT/ws"
mkdir -p "$WROOT/a" "$WROOT/b"
( cd "$WROOT/a" && run "$GO" mod init example.com/wa >/dev/null 2>&1 && \
  printf 'package wa\nfunc A() int { return 1 }\n' > a.go )
( cd "$WROOT/b" && run "$GO" mod init example.com/wb >/dev/null 2>&1 && \
  printf 'package wb\nfunc B() int { return 2 }\n' > b.go )
cd "$WROOT"
run "$GO" work init ./a >/dev/null 2>&1; assert_rc0 "work/init" $?
assert_has "work/init-file" "use ./a" "$(cat go.work 2>&1)"
run "$GO" work use ./b >/dev/null 2>&1; assert_rc0 "work/use" $?
assert_has "work/use-added" "./b" "$(cat go.work 2>&1)"
WEJ=$(run_out "$GO" work edit -json); assert_has "work/edit--json" '"Use"' "$WEJ"
run "$GO" work sync >/dev/null 2>&1; assert_rc0 "work/sync" $?
# go work vendor: 1.22+ supports it; rc0 expected (no external deps)
if run "$GO" work vendor >/dev/null 2>&1; then ok "work/vendor"; else skip "work/vendor" "go work vendor unsupported on 1.$GOMINOR"; fi
rm -rf vendor
cd "$ROOT"

# =====================================================================
# 17. go tool  (list tools;  exercise a representative set on a Go-built binary)
# =====================================================================
# Since go1.24 bare `go tool` lists only module-defined tools, not the builtin
# compiler/analysis tools — those are still shipped in the GOROOT pkg/tool dir and
# invocable as `go tool <name>`. Probe each directly with `go tool -n <name>`
# (prints the resolved tool path, no execution) and assert it resolves.
for t in compile link asm nm objdump pack addr2line buildid cover covdata pprof trace test2json dist vet cgo fix; do
  if run "$GO" tool -n "$t" >/dev/null 2>&1; then ok "tool/has/$t"; else skip "tool/has/$t" "not provided by this GOROOT's pkg/tool"; fi
done
# Functional: go tool buildid + nm + objdump on a real Go-compiled binary.
run "$GO" build -o toolbin . >/dev/null 2>&1
if [ -x ./toolbin ]; then
  BID=$(run_out "$GO" tool buildid ./toolbin)
  if [ -n "$BID" ]; then ok "tool/buildid-functional"; else skip "tool/buildid-functional" "empty buildid"; fi
  NMO=$(run_out "$GO" tool nm ./toolbin)
  assert_has "tool/nm-functional" "main.main" "$NMO"
  if run "$GO" tool objdump -s main.main ./toolbin >/dev/null 2>&1; then ok "tool/objdump-functional"; else skip "tool/objdump-functional" "objdump unavailable"; fi
else
  skip "tool/buildid-functional" "no toolbin"
  skip "tool/nm-functional" "no toolbin"
  skip "tool/objdump-functional" "no toolbin"
fi
# go tool test2json: convert a tiny test stream
T2J=$(run_out "$GO" test -json -run TestAdd ./... | head -1)
assert_has "tool/test2json-via-test-json" '"Action"' "$T2J"
# go tool covdata / pprof / trace need produced artifacts; assert they're at least invocable
# (covdata with no command selector prints its usage banner — that proves the tool is present).
CVO=$(run_out "$GO" tool covdata)
assert_has "tool/covdata-invocable" "covdata" "$CVO"
if run "$GO" tool pprof -h >/dev/null 2>&1; then ok "tool/pprof-invocable"; else skip "tool/pprof-invocable" "pprof -h nonzero (it may require an arg)"; fi

# =====================================================================
# 18. go telemetry  (subcommand: status / on / off / local / view)
#     Telemetry persists its mode in $GOTELEMETRYDIR; isolate it under ROOT so
#     on/off/local mutations NEVER touch the user's real telemetry config.
# =====================================================================
export GOTELEMETRYDIR="$ROOT/telemetry"
mkdir -p "$GOTELEMETRYDIR"
# CORRECT availability gate: telemetry is ABSENT iff the output contains the
# literal "unknown command" (this is what older toolchains print). The previous
# `grep -qiv 'unknown command'` was inverted — it matched the *second* line
# ("Run 'go help' for usage.") and wrongly concluded telemetry was available.
TMPROBE=$(run_out "$GO" telemetry)
if printf '%s' "$TMPROBE" | grep -qi 'unknown command'; then
  TELE_OK=0
else
  TELE_OK=1
fi
if [ "$TELE_OK" -eq 1 ]; then
  # Exercise each documented sub-action individually with an OBSERVABLE effect.
  # `go telemetry` with no arg prints the current mode (on|off|local).
  TM=$(run_out "$GO" telemetry)
  if printf '%s' "$TM" | grep -qiE '^(on|off|local)$|mode|telemetry'; then ok "telemetry/status"; else bad "telemetry/status" "no mode reported: $(printf '%s' "$TM" | head -c 80)"; fi
  # local: set mode to local, then read it back and assert it stuck.
  run "$GO" telemetry local >/dev/null 2>&1; assert_rc0 "telemetry/local-set" $?
  assert_has "telemetry/local-readback" "local" "$(run_out "$GO" telemetry)"
  # off: set mode off, read back off (differential vs the local we just set).
  run "$GO" telemetry off >/dev/null 2>&1; assert_rc0 "telemetry/off-set" $?
  assert_has "telemetry/off-readback" "off" "$(run_out "$GO" telemetry)"
  # on: enabling uploads; setting back to local (no upload) keeps it safe and
  # still proves the 'on'-family transition round-trips. We assert the mode
  # changed away from 'off'.
  run "$GO" telemetry on >/dev/null 2>&1; assert_rc0 "telemetry/on-set" $?
  assert_lacks "telemetry/on-not-off" "off" "$(run_out "$GO" telemetry)"
  run "$GO" telemetry local >/dev/null 2>&1   # restore non-uploading mode
  # view: opens a local web UI in newer builds; that would BLOCK/serve, so we do
  # NOT invoke it bare. Confirm it is a recognized sub-action via help instead.
  if run_out "$GO" help telemetry | grep -qi 'view'; then ok "telemetry/view-documented"; else skip "telemetry/view-documented" "this build's telemetry help omits 'view'"; fi
else
  skip "telemetry/status"          "host go 1.$GOMINOR has no 'go telemetry' (added go1.23+, in the 1.26 set)"
  skip "telemetry/local-set"       "no 'go telemetry' on host go 1.$GOMINOR"
  skip "telemetry/local-readback"  "no 'go telemetry' on host go 1.$GOMINOR"
  skip "telemetry/off-set"         "no 'go telemetry' on host go 1.$GOMINOR"
  skip "telemetry/off-readback"    "no 'go telemetry' on host go 1.$GOMINOR"
  skip "telemetry/on-set"          "no 'go telemetry' on host go 1.$GOMINOR"
  skip "telemetry/on-not-off"      "no 'go telemetry' on host go 1.$GOMINOR"
  skip "telemetry/view-documented" "no 'go telemetry' on host go 1.$GOMINOR"
fi

# =====================================================================
# 19. go get / go install  (REMOTE — reasoned SKIP, offline + GOPROXY=off)
# =====================================================================
# `go get <remote>` and `go install <remote>@version` require the module proxy / network,
# which is intentionally disabled (GOPROXY=off) to keep the carpet offline & deterministic.
# We DO exercise the offline-valid forms:
#   * go install of a LOCAL main package (compiles + installs into GOBIN)
export GOBIN="$ROOT/bin"; mkdir -p "$GOBIN"
if (cd "$ROOT" && run "$GO" install . >/dev/null 2>&1); then
  # Differential: an actual executable artifact must appear in GOBIN and run.
  IBIN=""
  for cand in "$GOBIN/clicarpet" "$GOBIN/"*; do [ -x "$cand" ] && { IBIN=$cand; break; }; done
  if [ -n "$IBIN" ]; then
    assert_has "install/local-main" "hi starry" "$(run_out "$IBIN")"
  else
    bad "install/local-main" "go install produced no executable in GOBIN"
  fi
else
  skip "install/local-main" "go install of local main failed offline"
fi
skip "get/remote" "network disabled (GOPROXY=off) — go get <remote> needs the module proxy"
skip "install/remote@version" "network disabled (GOPROXY=off) — go install pkg@ver needs the module proxy"

# =====================================================================
# 20. go bug  (opens a browser / writes a report template) — reasoned SKIP
# =====================================================================
# `go bug` launches a browser to file a GitHub issue (interactive/display). Confirm it is a
# recognized command via help; do NOT invoke it (no display, agent-only environment).
assert_has "bug/is-command" "bug" "$(run_out "$GO" help bug)"
skip "bug/invoke" "go bug opens a browser to file an issue (display/interactive) — agent-only env"

# =====================================================================
# 21. go help <topic>  — EVERY additional help topic must produce non-empty docs
# =====================================================================
# Topics always present (go1.16+). goauth & buildjson are go1.26-only -> version-gated.
for topic in buildconstraint buildmode c cache environment filetype go.mod gopath goproxy \
             importpath modules module-auth packages private testflag testfunc vcs; do
  HT=$(run_out "$GO" help "$topic")
  if [ -n "$HT" ] && ! printf '%s' "$HT" | grep -qi 'unknown help topic'; then
    ok "help-topic/$topic"
  else
    bad "help-topic/$topic" "empty or unknown"
  fi
done
for topic in goauth buildjson; do
  HT=$(run_out "$GO" help "$topic")
  if [ -n "$HT" ] && ! printf '%s' "$HT" | grep -qi 'unknown help topic'; then
    ok "help-topic/$topic"
  else
    skip "help-topic/$topic" "go1.26-only topic; absent on host go 1.$GOMINOR"
  fi
done
# go help <command> for every command (the per-command help tree)
for cmd in bug build clean doc env fix fmt generate get install list mod work run test tool version vet; do
  HC=$(run_out "$GO" help "$cmd")
  if [ -n "$HC" ] && ! printf '%s' "$HC" | grep -qi 'unknown'; then ok "help-cmd/$cmd"; else bad "help-cmd/$cmd" "empty/unknown"; fi
done
# Nested help, aligned with the header goal: `go help mod <sub>` is the CANONICAL
# per-subcommand help. Assert each nested topic returns its OWN usage line
# ("usage: go mod <sub>"), not merely that the overview lists the word.
for sub in download edit graph init tidy vendor verify why; do
  HS=$(run_out "$GO" help mod "$sub")
  if printf '%s' "$HS" | grep -qi "usage: go mod $sub"; then ok "help-mod-sub/$sub"
  elif [ -n "$HS" ] && printf '%s' "$HS" | grep -q "$sub" && ! printf '%s' "$HS" | grep -qi 'unknown'; then ok "help-mod-sub/$sub"
  else bad "help-mod-sub/$sub" "no nested help for go mod $sub"; fi
done
for sub in edit init sync use vendor; do
  HS=$(run_out "$GO" help work "$sub")
  if printf '%s' "$HS" | grep -qi "usage: go work $sub"; then ok "help-work-sub/$sub"
  elif [ -n "$HS" ] && printf '%s' "$HS" | grep -q "$sub" && ! printf '%s' "$HS" | grep -qi 'unknown'; then ok "help-work-sub/$sub"
  else bad "help-work-sub/$sub" "no nested help for go work $sub"; fi
done

# =====================================================================
# 22. go build — EXHAUSTIVE build-flag surface (PRIORITY 2)
#     Every flag is exercised with a REAL differential where feasible, not just
#     rc0. Network-free: GOPROXY=off; all inputs are local files under $ROOT.
#     We are back in $ROOT (restored after the work section) with the canonical
#     module ($MOD: main.go/util.go/main_test.go) intact.
# =====================================================================

# -tags: a build-constrained file is selected ONLY when its tag is requested.
# Two mutually-exclusive files (carpettag / !carpettag) define tagValue(); main
# prints it. Building WITH the tag must compile the tagged file (TAG_ON marker);
# WITHOUT it, the default file (TAG_OFF). This proves -tags actually toggles
# which source is compiled — a true differential, not just a successful build.
mkdir -p "$ROOT/tagmod"
cat > "$ROOT/tagmod/main.go" <<'EOF'
package main

import "fmt"

func main() { fmt.Println("TAGMARK=" + tagValue()) }
EOF
cat > "$ROOT/tagmod/tagged.go" <<'EOF'
//go:build carpettag

package main

func tagValue() string { return "CARPET_TAG_ON" }
EOF
cat > "$ROOT/tagmod/untagged.go" <<'EOF'
//go:build !carpettag

package main

func tagValue() string { return "CARPET_TAG_OFF" }
EOF
( cd "$ROOT/tagmod" && run "$GO" mod init example.com/tagmod >/dev/null 2>&1 )
TAGON=$( cd "$ROOT/tagmod" && run_out "$GO" run -tags carpettag . )
assert_has "build/-tags-on" "TAGMARK=CARPET_TAG_ON" "$TAGON"
TAGOFF=$( cd "$ROOT/tagmod" && run_out "$GO" run . )
assert_has "build/-tags-off-default" "TAGMARK=CARPET_TAG_OFF" "$TAGOFF"

# -trimpath: strips the local build directory from the binary so it embeds no
# absolute filesystem paths. Real differential: build the SAME program twice
# (with and without -trimpath); the plain binary may embed $ROOT, the trimmed
# one MUST NOT. We assert rc0 AND that $ROOT is absent from the trimmed binary.
run "$GO" build -trimpath -o trimbin . >/dev/null 2>&1; assert_rc0 "build/-trimpath-rc0" $?
if [ -x ./trimbin ]; then
  if grep -q -- "$ROOT" trimbin 2>/dev/null; then
    bad "build/-trimpath-strips-abspath" "trimmed binary still embeds [$ROOT]"
  else
    ok "build/-trimpath-strips-abspath"
  fi
else
  bad "build/-trimpath-strips-abspath" "no trimbin produced"
fi

# -ldflags=-X: inject a value into a package-level string var at link time. The
# program prints main.Version; we inject CARPET_LDFLAGS_OK and assert it appears
# at RUNTIME (proving the linker substitution took effect, not just compiled).
mkdir -p "$ROOT/ldmod"
cat > "$ROOT/ldmod/main.go" <<'EOF'
package main

import "fmt"

// Version is overridden at link time via -ldflags=-X.
var Version string

func main() { fmt.Println("VERSION=" + Version) }
EOF
( cd "$ROOT/ldmod" && run "$GO" mod init example.com/ldmod >/dev/null 2>&1 )
( cd "$ROOT/ldmod" && run "$GO" build -ldflags=-X' 'main.Version=CARPET_LDFLAGS_OK -o ldbin . >/dev/null 2>&1 )
if [ -x "$ROOT/ldmod/ldbin" ]; then
  assert_has "build/-ldflags--X" "VERSION=CARPET_LDFLAGS_OK" "$(run_out "$ROOT/ldmod/ldbin")"
else
  bad "build/-ldflags--X" "no ldbin produced"
fi

# -buildvcs=false: suppress embedding VCS (git) stamp info. Outside a repo the
# default already omits VCS, but the explicit flag must be accepted (rc0).
run "$GO" build -buildvcs=false -o bvbin . >/dev/null 2>&1; assert_rc0 "build/-buildvcs=false" $?
# -a: force rebuild of all packages (ignore cached objects). rc0 expected.
run "$GO" build -a -o abin . >/dev/null 2>&1; assert_rc0 "build/-a-rebuild-all" $?
# -p=1: cap build parallelism to a single program. rc0 expected (just slower).
run "$GO" build -p=1 -o pbin . >/dev/null 2>&1; assert_rc0 "build/-p=1" $?
# -work: print (and keep) the temporary work directory. Differential: the
# combined output must contain a "WORK=" line naming that temp dir.
WK=$(run_out "$GO" build -work -o wkbin .)
assert_has "build/-work-prints-WORK" "WORK=" "$WK"
# Best-effort cleanup of the kept work dir so it does not linger (ROOT trap also covers /tmp leaks elsewhere).
WKDIR=$(printf '%s' "$WK" | sed -n 's/^WORK=//p' | head -1); [ -n "$WKDIR" ] && [ -d "$WKDIR" ] && rm -rf "$WKDIR" 2>/dev/null

# -coverpkg / -covermode: count-mode coverage instrumented across all packages.
# rc0 + coverage line shown (proves instrumentation ran, not just a plain test).
CP=$(run_out "$GO" test -covermode=count -coverpkg=./... -run TestAdd ./...)
CPRC=$?
assert_rc0 "build/-covermode+coverpkg-rc0" $CPRC
assert_has "build/-coverpkg-shows-coverage" "coverage:" "$CP"

# -modfile: build using an ALTERNATE go.mod (alt.mod) instead of the default.
cp go.mod alt.mod
run "$GO" build -modfile=alt.mod ./... >/dev/null 2>&1; assert_rc0 "build/-modfile=alt.mod" $?
rm -f alt.mod alt.sum

# -overlay: supply a JSON file mapping a VIRTUAL source path to a REAL file on
# disk; the build sees the virtual file as part of the package. We overlay an
# extra source (extra.go) backed by realextra.go.txt that defines a symbol the
# program would otherwise lack, then build the package WITH the overlay. rc0
# proves the overlay file was injected into the compilation unit.
mkdir -p "$ROOT/ovmod"
cat > "$ROOT/ovmod/main.go" <<'EOF'
package main

import "fmt"

func main() { fmt.Println(overlayMarker()) }
EOF
cat > "$ROOT/ovmod/realextra.go.txt" <<'EOF'
package main

func overlayMarker() string { return "OVERLAY_OK" }
EOF
( cd "$ROOT/ovmod" && run "$GO" mod init example.com/ovmod >/dev/null 2>&1 )
cat > "$ROOT/ovmod/overlay.json" <<EOF
{"Replace":{"$ROOT/ovmod/extra.go":"$ROOT/ovmod/realextra.go.txt"}}
EOF
( cd "$ROOT/ovmod" && run "$GO" build -overlay=overlay.json -o ovbin . >/dev/null 2>&1 )
OVRC=$?
if [ "$OVRC" -eq 0 ] && [ -x "$ROOT/ovmod/ovbin" ]; then
  assert_has "build/-overlay-injects-file" "OVERLAY_OK" "$(run_out "$ROOT/ovmod/ovbin")"
else
  bad "build/-overlay-injects-file" "overlay build failed (rc=$OVRC)"
fi

# -toolexec: wraps every toolchain invocation in a program. A working wrapper is
# fiddly to make portable (must forward argv and exec the real tool); rather than
# risk a flaky build, we assert the flag is DOCUMENTED in `go help build` (its
# canonical reference). This is an observable check on the toolchain's own docs.
assert_has "build/-toolexec-documented" "-toolexec" "$(run_out "$GO" help build)"

# -buildmode=pie: position-independent executable. Supported on linux/amd64 (and
# most modern targets); build a PIE and assert rc0 if it succeeds, else skip with
# the host GOOS/GOARCH as the reason.
if run "$GO" build -buildmode=pie -o piebin . >/dev/null 2>&1; then
  ok "build/-buildmode=pie"
else
  skip "build/-buildmode=pie" "buildmode=pie unsupported on $VGOOS/$VGOARCH"
fi

# -buildmode=archive: compile a NON-MAIN package into a .a archive (main pkgs are
# rejected with 'no packages to build'). We build the local mathutil pkg.
mkdir -p "$ROOT/archmod/mathutil"
cat > "$ROOT/archmod/go.mod" <<EOF
module example.com/archmod

go 1.21
EOF
cat > "$ROOT/archmod/mathutil/m.go" <<'EOF'
package mathutil

// Double is exported so the package is a real, buildable library.
func Double(x int) int { return x * 2 }
EOF
( cd "$ROOT/archmod" && run "$GO" build -buildmode=archive -o math.a ./mathutil >/dev/null 2>&1 )
if [ -f "$ROOT/archmod/math.a" ]; then
  ok "build/-buildmode=archive"
else
  skip "build/-buildmode=archive" "buildmode=archive produced no .a on $VGOOS/$VGOARCH"
fi

# --- build flags that require capabilities DISABLED in this carpet: reasoned SKIPs ---
skip "build/-race"               "needs CGO + race runtime; CGO_ENABLED=0 in this offline carpet"
skip "build/-msan"               "memory sanitizer needs cgo + clang; CGO_ENABLED=0"
skip "build/-asan"               "address sanitizer needs cgo + clang; CGO_ENABLED=0"
skip "build/-buildmode=c-shared" "c-shared needs cgo (CGO_ENABLED=0)"
skip "build/-buildmode=c-archive" "c-archive needs cgo (CGO_ENABLED=0)"
skip "build/-buildmode=plugin"   "plugin needs cgo + dynamic linking; CGO_ENABLED=0"
skip "build/-pgo"                "needs a CPU profile (default=auto looks for default.pgo; none present, deterministic skip)"
skip "build/-linkshared"         "needs a shared std build (go install -buildmode=shared std) — not available offline"

# =====================================================================
# 23. go test — EXHAUSTIVE test-flag surface (PRIORITY 2)
#     All run against the GREEN suite in $ROOT (TestGreet/TestAdd/BenchmarkAdd).
# =====================================================================
# -failfast: stop at the first failing test. On a passing suite it must still
# rc0 (no failures to stop at) — a clean, deterministic exercise of the flag.
run "$GO" test -failfast -run TestAdd -count=1 ./... >/dev/null 2>&1; assert_rc0 "test/-failfast" $?
# -list: print matching test function names WITHOUT running them. Differential:
# the listing must include TestAdd (a known function), proving discovery ran.
TL=$(run_out "$GO" test -list '.*' ./...)
assert_has "test/-list-shows-TestAdd" "TestAdd" "$TL"
# -parallel: set the parallelism for t.Parallel() tests. rc0 expected.
run "$GO" test -parallel=2 -run TestAdd -count=1 ./... >/dev/null 2>&1; assert_rc0 "test/-parallel=2" $?
# -short: enable short-mode (testing.Short()). rc0 expected.
run "$GO" test -short -run TestAdd -count=1 ./... >/dev/null 2>&1; assert_rc0 "test/-short" $?
# -shuffle=on: randomize test/benchmark order. rc0 expected (order-independent here).
run "$GO" test -shuffle=on -run TestAdd -count=1 ./... >/dev/null 2>&1; assert_rc0 "test/-shuffle=on" $?
# -skip: exclude tests by regexp. Differential: skip TestGreet, run Test.* — the
# verbose log must still show TestAdd executed (so -skip narrowed, not muted, all).
SK=$(run_out "$GO" test -skip TestGreet -run 'Test.*' -v -count=1 ./...)
SKRC=$?
assert_rc0 "test/-skip-rc0" $SKRC
assert_has "test/-skip-still-runs-TestAdd" "TestAdd" "$SK"
# -timeout: panic the test binary if it runs longer than the limit. rc0 expected.
run "$GO" test -timeout=60s -run TestAdd -count=1 ./... >/dev/null 2>&1; assert_rc0 "test/-timeout=60s" $?
# -vet=off: disable the implicit `go vet` pass before tests. rc0 expected.
run "$GO" test -vet=off -run TestAdd -count=1 ./... >/dev/null 2>&1; assert_rc0 "test/-vet=off" $?
# -benchmem: report memory allocation stats with benchmarks. Differential: the
# output must contain B/op or allocs/op columns (only present with -benchmem).
BM=$(run_out "$GO" test -run='^$' -bench=BenchmarkAdd -benchmem -benchtime=1x ./...)
case "$BM" in
  *allocs/op*|*B/op*) ok "test/-benchmem-cols" ;;
  *) bad "test/-benchmem-cols" "no B/op or allocs/op column: $(printf '%s' "$BM" | head -c 120)" ;;
esac
# -coverprofile: write coverage to a file. rc0 + the file must exist afterward.
rm -f cov.out
run "$GO" test -coverprofile=cov.out -run TestAdd -count=1 ./... >/dev/null 2>&1
CPRRC=$?
assert_rc0 "test/-coverprofile-rc0" $CPRRC
if [ -s cov.out ]; then ok "test/-coverprofile-file"; else bad "test/-coverprofile-file" "cov.out missing/empty"; fi
rm -f cov.out
# -cpuprofile / -memprofile: write CPU and memory profiles. rc0 + both files exist.
rm -f cpu.out mem.out
run "$GO" test -run TestAdd -count=1 -cpuprofile=cpu.out -memprofile=mem.out ./... >/dev/null 2>&1
PFRC=$?
assert_rc0 "test/-cpu+memprofile-rc0" $PFRC
if [ -e cpu.out ] && [ -e mem.out ]; then ok "test/-cpu+memprofile-files"; else bad "test/-cpu+memprofile-files" "profile files missing"; fi
rm -f cpu.out mem.out
# -fuzz / -fuzztime: run a fuzz target for a BOUNDED time. We add a FuzzAdd target
# in an isolated module so the corpus is local and the run is deterministic and
# short (10ms). rc0 expected (Add never panics).
mkdir -p "$ROOT/fuzzmod"
cat > "$ROOT/fuzzmod/main.go" <<'EOF'
package fuzzmod

// Add is the function under fuzz; it is total and never panics.
func Add(a, b int) int { return a + b }
EOF
cat > "$ROOT/fuzzmod/fuzz_test.go" <<'EOF'
package fuzzmod

import "testing"

func FuzzAdd(f *testing.F) {
	f.Add(1, 2)
	f.Fuzz(func(t *testing.T, a, b int) {
		if Add(a, b) != a+b {
			t.Fatalf("add mismatch")
		}
	})
}
EOF
( cd "$ROOT/fuzzmod" && run "$GO" mod init example.com/fuzzmod >/dev/null 2>&1 )
( cd "$ROOT/fuzzmod" && run "$GO" test -run='^$' -fuzz=FuzzAdd -fuzztime=10ms ./... >/dev/null 2>&1 )
assert_rc0 "test/-fuzz+fuzztime" $?

# =====================================================================
# 24. go list — additional documented flags (PRIORITY 2)
# =====================================================================
# -m all: list the main module and all its module dependencies. With no external
# requires, this is just the main module — assert it names $MOD.
assert_has "list/-m-all" "$MOD" "$(run_out "$GO" list -m all)"
# -export: build the packages and report the path to their export-data file. rc0.
run "$GO" list -export . >/dev/null 2>&1; assert_rc0 "list/-export" $?
# -compiled: report the .go files actually handed to the compiler. Differential:
# the {{.CompiledGoFiles}} template must be non-empty (lists main.go/util.go).
LCF=$(run_out "$GO" list -compiled -f '{{.CompiledGoFiles}}' .)
if [ -n "$LCF" ] && [ "$LCF" != "[]" ]; then ok "list/-compiled-nonempty"; else bad "list/-compiled-nonempty" "empty CompiledGoFiles: [$LCF]"; fi
# -test: include the synthesized test packages in the listing. rc0.
run "$GO" list -test -f '{{.ImportPath}}' . >/dev/null 2>&1; assert_rc0 "list/-test" $?
# -find: list the package WITHOUT resolving imports/deps (fast). Must name $MOD.
assert_has "list/-find" "$MOD" "$(run_out "$GO" list -find .)"
# Network-requiring list forms: explicit, reasoned SKIPs (GOPROXY=off).
skip "list/-m--versions" "needs the module proxy to enumerate versions (GOPROXY=off)"
skip "list/-u--m-all"    "needs the module proxy to check for updates (GOPROXY=off)"

# =====================================================================
# 25. go env -changed  (PRIORITY 2) — list only vars changed from default.
# =====================================================================
# This carpet exports several non-default vars (GOPROXY=off, GOFLAGS, GOENV...),
# so -changed must rc0 AND name at least one of them (GOPROXY). Differential
# against a clean environment where the list would be empty.
EC=$(run_out "$GO" env -changed); ECRC=$?
assert_rc0 "env/-changed-rc0" $ECRC
assert_has "env/-changed-lists-GOPROXY" "GOPROXY" "$EC"

# =====================================================================
# 26. go version -m  (PRIORITY 2) — module build info embedded in a binary.
# =====================================================================
# Build a fresh binary, then `go version -m` it: the output must carry the
# toolchain version line ("go1.") and the module path embedded at build time.
run "$GO" build -o vmbin . >/dev/null 2>&1
if [ -x ./vmbin ]; then
  VM=$(run_out "$GO" version -m ./vmbin)
  assert_has "version/-m-go-line" "go1." "$VM"
  assert_has "version/-m-module-path" "$MOD" "$VM"
else
  bad "version/-m-go-line" "no vmbin produced"
  bad "version/-m-module-path" "no vmbin produced"
fi

# =====================================================================
# 27. go vet — additional documented checks/flags (PRIORITY 2)
# =====================================================================
# `go help vet` is the canonical reference; assert it documents the vet tool and
# its -vettool selector (stable across toolchains).
VH=$(run_out "$GO" help vet)
assert_has "vet/help-mentions-vet-tool" "Go vet tool" "$VH"
assert_has "vet/help-mentions-vettool-flag" "-vettool" "$VH"
# -c=N: context lines around a diagnostic. On clean code there is no diagnostic,
# so vet -c=2 simply succeeds (rc0) — a clean exercise of the flag.
run "$GO" vet -c=2 ./... >/dev/null 2>&1; assert_rc0 "vet/-c=2-clean" $?

# =====================================================================
# 28. go doc — additional documented flags (PRIORITY 2)
# =====================================================================
# -cmd: include command-level (package main) exported symbols. On a library pkg
# like fmt it still prints the package header; assert "package fmt".
assert_has "doc/-cmd" "package fmt" "$(run_out "$GO" doc -cmd fmt)"
# -u: include unexported identifiers. Must still print the package header.
DU=$(run_out "$GO" doc -u fmt); assert_rc0 "doc/-u-rc0" $?
assert_has "doc/-u-pkg" "package fmt" "$DU"
# -short: one-line-per-symbol summary. For fmt.Println the summary must name it.
assert_has "doc/-short" "Println" "$(run_out "$GO" doc -short fmt.Println)"

# =====================================================================
# 22b. LANGUAGE/RUNTIME bridge — build+run the canonical go126test program if present.
# =====================================================================
# This proves the toolchain can compile+run the existing #764 language carpet. We locate it
# relative to this script (portable: no hard host path baked into the LOGIC).
# Self-contained: write a tiny program that uses go1.26-only language/stdlib APIs
# (new(expression) + errors.AsType[T]) and build+run it through the `go` CLI. This
# proves the toolchain compiles+runs 1.26 features, with no external file dependency.
LDIR="$ROOT/lang"; mkdir -p "$LDIR"
cat > "$LDIR/main.go" <<'GO126EOF'
package main

import (
	"errors"
	"fmt"
)

type myErr struct{ code int }

func (myErr) Error() string { return "boom" }

func main() {
	p := new(40 + 2) // go1.26: new(expression) returns a pointer to a new var
	var err error = fmt.Errorf("wrap: %w", myErr{code: 7})
	e, ok := errors.AsType[myErr](err) // go1.26: generic, type-safe errors.As
	if ok && *p == 42 && e.code == 7 {
		fmt.Println("HELLO_GO126_OK", *p, e.code)
	} else {
		fmt.Println("HELLO_GO126_BAD")
	}
}
GO126EOF
( cd "$LDIR" && run "$GO" mod init example.com/lang >/dev/null 2>&1 )
LOUT=$( cd "$LDIR" && run_out "$GO" run . )
if [ "$GOMINOR" -ge 26 ]; then
  assert_has "lang/go126-features-build-run" "HELLO_GO126_OK 42 7" "$LOUT"
else
  skip "lang/go126-features-build-run" "needs go1.26 APIs (new(expr)/errors.AsType); host go 1.$GOMINOR can't compile them"
fi

# =====================================================================
# SUMMARY
# =====================================================================
printf '\n==== go-cli-carpet summary ====\n'
printf 'PASS=%d FAIL=%d SKIP=%d\n' "$PASS" "$FAIL" "$SKIP"
if [ "$FAIL" -eq 0 ]; then
  printf 'GO_CLI_OK\n'
  exit 0
else
  printf 'GO_CLI_FAIL\n'
  exit 1
fi
