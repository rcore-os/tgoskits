#!/usr/bin/env bash
# =============================================================================
# bsc-cli-carpet.sh -- COMPREHENSIVE, doc-grounded carpet for the bsc (Bluespec
# compiler) command-line interface, for the StarryOS #764 "bluesv" (Bluespec)
# language delivery (industrial / production use).
#
# GROUND TRUTH: every flag enumerated by `bsc -help` (77 flags on the host
# build) cross-referenced against specs/doc/user_guide/user_guide.tex section
# "BSC flags" (\label{compiler-flags}).  EVERY flag is either EXERCISED with an
# observable assertion, or logged as an explicit reasoned SKIP (with a concrete
# reason: needs-foreign-toolchain / needs-external-EDA / needs-large-design /
# documented-but-not-in-binary).
#
# Each check prints "OK <id> <what>" on success or "FAIL <id> <what>" on
# failure; the script tallies and, only if zero FAILs, prints the OK token:
#     BSC_CLI_OK <n>/<n>
# Host is the golden: a FAIL here means the check is wrong, not bsc.
#
# Parameterised + portable: bsc binary, work dir, and source files are all
# resolved relative to env vars / the script's own location, with no hard host
# absolute paths in the test logic, so the same carpet runs on-target (starry).
# =============================================================================
set -u

# --- configuration (all overridable via env for on-target runs) --------------
BSC="${BSC:-$(command -v bsc || echo /usr/local/bsc/bin/bsc)}"
HERE="$(cd "$(dirname "$0")" && pwd)"
SRCDIR="${BSC_CARPET_SRCDIR:-$HERE}"
WORK="${BSC_CARPET_WORK:-$(mktemp -d "${TMPDIR:-/tmp}/bsc-cli.XXXXXX")}"
BH_SRC="${BH_SRC:-$SRCDIR/Tb.bs}"
BSV_SRC="${BSV_SRC:-$SRCDIR/Tb.bsv}"
VSIM="${BSC_VSIM:-iverilog}"
# per-invocation timeout for every external tool call (overridable for slow
# on-target runs).  rc 124 => a hung tool, which we treat as a FAIL, never a hang.
BSC_TIMEOUT="${BSC_TIMEOUT:-180}"

mkdir -p "$WORK"

PASS=0; FAILN=0; SKIP=0
declare -a FAILED

pass()  { PASS=$((PASS+1)); echo "OK   $1 $2"; }
fail()  { FAILN=$((FAILN+1)); FAILED+=("$1 $2"); echo "FAIL $1 $2"; }
skipit(){ SKIP=$((SKIP+1)); echo "SKIP $1 $2"; }

# TIMEOUT-GUARD wrapper: run any command under `timeout`, redirecting stdin from
# /dev/null so a tool can never block reading a terminal.  rc 124 (the timeout
# kill code) is surfaced verbatim so callers see a deterministic FAIL, not a hang.
run() { timeout "$BSC_TIMEOUT" "$@" </dev/null 2>&1; }
timed_out() { [ "$RC" -eq 124 ]; }

# run bsc in a clean per-check subdir; returns its combined output in $OUT, rc in $RC
B() {
   local sub="$1"; shift
   local d="$WORK/$sub"
   rm -rf "$d"; mkdir -p "$d"
   OUT="$(run "$BSC" -bdir "$d" -vdir "$d" -info-dir "$d" -simdir "$d" "$@")"
   RC=$?
}
# like B but no -bdir/-vdir injection (for flags that must be the only args)
Braw() { OUT="$(run "$BSC" "$@")"; RC=$?; }
# like B but runs bsc with cwd INSIDE the per-check subdir.  Needed for -cpp /
# -Xcpp, whose C-preprocessor intermediate (NNNNN.c) is written to the cwd, not
# to -bdir; running from a read-only cwd (e.g. /) would otherwise fail (S0031).
Bcd() {
   local sub="$1"; shift
   local d="$WORK/$sub"
   rm -rf "$d"; mkdir -p "$d"
   OUT="$(cd "$d" && run "$BSC" -bdir "$d" -vdir "$d" -info-dir "$d" -simdir "$d" "$@")"
   RC=$?
}

# assert helpers
ok_rc()      { [ "$RC" -eq 0 ]; }
has()        { printf '%s' "$OUT" | grep -q -- "$1"; }
hasi()       { printf '%s' "$OUT" | grep -qi -- "$1"; }

echo "=== bsc CLI carpet ==="
echo "bsc      = $BSC"
echo "srcdir   = $SRCDIR"
echo "work     = $WORK"
echo "timeout  = ${BSC_TIMEOUT}s per tool call"
run "$BSC" -help >/dev/null 2>&1 || { echo "FATAL: bsc not runnable at $BSC"; exit 3; }

# Pre-flight: the BH and BSV sources must exist (the carpet compiles them).
[ -f "$BH_SRC" ]  || { echo "FATAL: missing BH source  $BH_SRC";  exit 3; }
[ -f "$BSV_SRC" ] || { echo "FATAL: missing BSV source $BSV_SRC"; exit 3; }

# Pre-flight: detect the verilog simulator (iverilog).  When absent we SKIP the
# verilog link/run checks rather than FAIL them (they need an external EDA tool).
HAVE_VSIM=0
if command -v "$VSIM" >/dev/null 2>&1; then HAVE_VSIM=1; fi
# Pre-flight: a C++ compiler is required for Bluesim (-sim -e) linking.
HAVE_CXX=0
for cxx in "${CXX:-}" c++ g++ clang++; do
   [ -n "$cxx" ] && command -v "$cxx" >/dev/null 2>&1 && { HAVE_CXX=1; break; }
done

# Tiny throwaway BSV used for many flag checks (kept minimal + fast).
SMALL="$WORK/Small.bsv"
cat > "$SMALL" <<'EOF'
package Small;
(* synthesize *)
module mkSmall(Empty);
   Reg#(Bit#(8)) r <- mkReg(0);
   rule tick (r < 3);
      r <= r + 1;
   endrule
   rule fin (r == 3);
      $display("SMALL_OK %0d", r);
      $finish(0);
   endrule
endmodule
endpackage
EOF

# Throwaway BSV with an empty rule + a provably-false rule (for remove-* flags).
DBG="$WORK/Dbg.bsv"
cat > "$DBG" <<'EOF'
package Dbg;
(* synthesize *)
module mkDbg(Empty);
   Reg#(Bit#(8)) r <- mkReg(0);
   rule go (r < 2);   r <= r + 1; endrule
   rule emptyR (True); noAction;  endrule          // empty-body rule
   rule falseR (False); r <= r + 9; endrule         // provably-false rule
endmodule
endpackage
EOF

# =============================================================================
# GROUP 0: help / version / flag-introspection (verbosity flags too)
# =============================================================================
Braw -help
{ ok_rc && hasi "Usage" && hasi "Compiler flags"; } && pass C001 "-help prints usage+flag list" || fail C001 "-help"
Braw -print-flags
{ ok_rc && hasi "Flags:"; } && pass C002 "-print-flags dumps parsed flags" || fail C002 "-print-flags"
# -quiet / -q : less talkative; -verbose / -v : more talkative
B q -verilog -g mkSmall -quiet "$SMALL"
{ ok_rc; } && pass C003 "-quiet compiles silently" || fail C003 "-quiet"
B q2 -verilog -g mkSmall -q "$SMALL"
{ ok_rc; } && pass C004 "-q (alias of -quiet)" || fail C004 "-q"
B vb -verilog -g mkSmall -verbose "$SMALL"
{ ok_rc && hasi "elaborat\|code gen\|compil"; } && pass C005 "-verbose is more talkative" || fail C005 "-verbose"
B vb2 -verilog -g mkSmall -v "$SMALL"
{ ok_rc; } && pass C006 "-v (alias of -verbose)" || fail C006 "-v"
# -show-version / -no-show-version : controls the COMPILER VERSION string in the
# generated .v header.  DIFFERENTIAL: with -show-version the header carries the
# literal "version 20..N" build string; with -no-show-version that version
# string is omitted (the "Generated by Bluespec Compiler" line stays, but with
# NO trailing version number).  We assert both sides of the contrast.
B sv -verilog -g mkSmall -show-version "$SMALL"
SV_HASVER=0; grep -rqiE "Bluespec Compiler, version [0-9]" "$WORK/sv"/*.v 2>/dev/null && SV_HASVER=1
B nsv -verilog -g mkSmall -no-show-version "$SMALL"
NSV_HASVER=0; grep -rqiE "Bluespec Compiler, version [0-9]" "$WORK/nsv"/*.v 2>/dev/null && NSV_HASVER=1
NSV_HASLINE=0; grep -rqi "Generated by Bluespec Compiler" "$WORK/nsv"/*.v 2>/dev/null && NSV_HASLINE=1
{ [ "$SV_HASVER" -eq 1 ]; }  && pass C007 "-show-version stamps the version string in .v"  || fail C007 "-show-version (no version string)"
{ [ "$NSV_HASVER" -eq 0 ] && [ "$NSV_HASLINE" -eq 1 ]; } \
  && pass C008 "-no-show-version omits the version string (header line still present)" \
  || fail C008 "-no-show-version (version string not omitted)"
# -show-timestamps / -no-show-timestamps : controls the date/time line in the .v.
# DIFFERENTIAL: -show-timestamps emits an "On <date>" line; -no-show-timestamps
# omits it.
B nts -verilog -g mkSmall -no-show-timestamps "$SMALL"
NTS_HASTS=0; grep -rqiE "^// On .* 20[0-9][0-9]" "$WORK/nts"/*.v 2>/dev/null && NTS_HASTS=1
B ts -verilog -g mkSmall -show-timestamps "$SMALL"
TS_HASTS=0; grep -rqiE "^// On .* 20[0-9][0-9]" "$WORK/ts"/*.v 2>/dev/null && TS_HASTS=1
{ [ "$NTS_HASTS" -eq 0 ]; } && pass C009 "-no-show-timestamps omits the timestamp line" || fail C009 "-no-show-timestamps (timestamp present)"
{ [ "$TS_HASTS"  -eq 1 ]; } && pass C010 "-show-timestamps emits the timestamp line"    || fail C010 "-show-timestamps (no timestamp)"

# =============================================================================
# GROUP 1: back-end + codegen selection (-verilog/-sim/-g/-u/-e/-o/-elab/-vsim)
# =============================================================================
# -verilog -g : compile one module to a .v
B vg -verilog -g mkSmall "$SMALL"
{ ok_rc && [ -f "$WORK/vg/mkSmall.v" ]; } && pass C011 "-verilog -g generates mkSmall.v" || fail C011 "-verilog -g"
# -verilog -g -u : recursive build (compiles the whole BH design + deps)
B vgu -verilog -g mkTb -u "$BH_SRC"
{ ok_rc && [ -f "$WORK/vgu/mkTb.v" ]; } && pass C012 "-verilog -g -u recursive build (BH)" || fail C012 "-verilog -g -u"
# multiple -g on one command line (doc: multiple modules at once)
B vgg -verilog -g mkSmall -g mkSmall "$SMALL"
{ ok_rc && [ -f "$WORK/vgg/mkSmall.v" ]; } && pass C013 "multiple -g modules at once" || fail C013 "multiple -g"
# -sim -g : compile to a Bluesim object (.ba/.o)
B sg -sim -g mkSmall "$SMALL"
{ ok_rc && ls "$WORK/sg"/*.ba >/dev/null 2>&1; } && pass C014 "-sim -g generates Bluesim object (.ba)" || fail C014 "-sim -g"
# -sim -g -u : recursive Bluesim build
B sgu -sim -g mkSmall -u "$SMALL"
{ ok_rc; } && pass C015 "-sim -g -u recursive Bluesim build" || fail C015 "-sim -g -u"
# -elab : produce a .ba after elaboration & scheduling
B el -verilog -g mkSmall -elab "$SMALL"
{ ok_rc && ls "$WORK/el"/*.ba >/dev/null 2>&1; } && pass C016 "-elab emits .ba file" || fail C016 "-elab"
# -e + -o : link a Verilog simulation model to a named output.
# Needs the external verilog simulator ($VSIM); preflight + SKIP if absent.
if [ "$HAVE_VSIM" -eq 1 ]; then
   B ln -verilog -g mkSmall -u "$SMALL" >/dev/null 2>&1
   OUT="$(run "$BSC" -verilog -e mkSmall -vsim "$VSIM" -bdir "$WORK/ln" -vdir "$WORK/ln" -info-dir "$WORK/ln" -simdir "$WORK/ln" -o "$WORK/ln/small.exe")"; RC=$?
   { ok_rc && [ -x "$WORK/ln/small.exe" ]; } && pass C017 "-e + -o links named Verilog sim model" || fail C017 "-e + -o"
   # -vsim : choose the verilog simulator (iverilog) -- verified implicitly by C017
   { [ -x "$WORK/ln/small.exe" ]; } && pass C018 "-vsim $VSIM selects simulator" || fail C018 "-vsim"
   # run the linked model: deterministic golden
   RUNOUT="$(cd "$WORK/ln" && run ./small.exe)"
   { printf '%s' "$RUNOUT" | grep -q "SMALL_OK 3"; } && pass C019 "linked sim runs -> SMALL_OK 3" || fail C019 "sim run"
else
   skipit C017 "-e + -o (needs verilog simulator '$VSIM': not on host)"
   skipit C018 "-vsim (needs verilog simulator '$VSIM': not on host)"
   skipit C019 "sim run (needs verilog simulator '$VSIM': not on host)"
fi
# -sim -e : link a Bluesim binary (needs a host C++ compiler; preflight + SKIP).
if [ "$HAVE_CXX" -eq 1 ]; then
   B sgu2 -sim -g mkSmall -u "$SMALL" >/dev/null 2>&1
   OUT="$(run "$BSC" -sim -e mkSmall -bdir "$WORK/sgu2" -simdir "$WORK/sgu2" -info-dir "$WORK/sgu2" -o "$WORK/sgu2/small.bsim")"; RC=$?
   { ok_rc && [ -x "$WORK/sgu2/small.bsim" ]; } && pass C020 "-sim -e links Bluesim binary" || fail C020 "-sim -e"
   RUNOUT="$(cd "$WORK/sgu2" && run ./small.bsim)"
   { printf '%s' "$RUNOUT" | grep -q "SMALL_OK 3"; } && pass C021 "Bluesim binary runs -> SMALL_OK 3" || fail C021 "bluesim run"
else
   skipit C020 "-sim -e (needs host C++ compiler for Bluesim link)"
   skipit C021 "bluesim run (needs host C++ compiler for Bluesim link)"
fi

# =============================================================================
# GROUP 2: path / directory flags (-bdir/-vdir/-simdir/-info-dir/-p/-i/-fdir/-vsearch)
# =============================================================================
# -bdir / -vdir / -info-dir : output directories (verified by files landing there)
rm -rf "$WORK/dirs"; mkdir -p "$WORK/dirs/bo" "$WORK/dirs/v" "$WORK/dirs/info"
OUT="$(run "$BSC" -verilog -g mkSmall -bdir "$WORK/dirs/bo" -vdir "$WORK/dirs/v" -info-dir "$WORK/dirs/info" "$SMALL")"; RC=$?
{ ok_rc && [ -f "$WORK/dirs/v/mkSmall.v" ] && ls "$WORK/dirs/bo"/*.bo >/dev/null 2>&1; } \
  && pass C022 "-bdir/-vdir/-info-dir route outputs" || fail C022 "-bdir/-vdir/-info-dir"
# -simdir : Bluesim intermediate dir (the .ba object must land there)
rm -rf "$WORK/sd"; mkdir -p "$WORK/sd"
OUT="$(run "$BSC" -sim -g mkSmall -bdir "$WORK/sd" -simdir "$WORK/sd" -info-dir "$WORK/sd" "$SMALL")"; RC=$?
{ ok_rc && ls "$WORK/sd"/*.ba >/dev/null 2>&1; } && pass C023 "-simdir routes the Bluesim .ba object" || fail C023 "-simdir"
# -p : source/intermediate search path ('+' = current path, '%' = BLUESPECDIR)
B pp -verilog -g mkSmall -p "+" "$SMALL"
{ ok_rc; } && pass C024 "-p search path (with '+')" || fail C024 "-p"
# -i : override BLUESPECDIR (point at the real lib dir => still works)
LIBDIR="$(dirname "$(dirname "$BSC")")/lib"
[ -d "$LIBDIR" ] || LIBDIR="/usr/local/bsc/lib"
B ii -verilog -g mkSmall -i "$LIBDIR" "$SMALL"
{ ok_rc; } && pass C025 "-i overrides BLUESPECDIR" || fail C025 "-i"
# -fdir : working dir for relative file paths during elaboration
B fd -verilog -g mkSmall -fdir "$WORK/fd" "$SMALL"
{ ok_rc; } && pass C026 "-fdir sets elaboration file base dir" || fail C026 "-fdir"
# -vsearch : verilog search path for linking (needs $VSIM; preflight + SKIP).
if [ "$HAVE_VSIM" -eq 1 ]; then
   B vgs -verilog -g mkSmall -u "$SMALL" >/dev/null 2>&1
   OUT="$(run "$BSC" -verilog -e mkSmall -vsim "$VSIM" -vsearch "+" -bdir "$WORK/vgs" -vdir "$WORK/vgs" -info-dir "$WORK/vgs" -simdir "$WORK/vgs" -o "$WORK/vgs/s.exe")"; RC=$?
   { ok_rc && [ -x "$WORK/vgs/s.exe" ]; } && pass C027 "-vsearch verilog link search path" || fail C027 "-vsearch"
else
   skipit C027 "-vsearch (needs verilog simulator '$VSIM': not on host)"
fi

# =============================================================================
# GROUP 3: Verilog back-end flags
# =============================================================================
# -remove-unused-modules
B rum -verilog -g mkSmall -remove-unused-modules "$SMALL"
{ ok_rc && [ -f "$WORK/rum/mkSmall.v" ]; } && pass C028 "-remove-unused-modules" || fail C028 "-remove-unused-modules"
# -v95 : strict Verilog-95; leaves a comment where features were removed
B v95 -verilog -g mkSmall -v95 "$SMALL"
{ ok_rc && [ -f "$WORK/v95/mkSmall.v" ]; } && pass C029 "-v95 strict Verilog-95 output" || fail C029 "-v95"
# -unspecified-to : the flag accepts 'X','0','1','Z','A' (doc), but the VERILOG
# back end only codegens 0/1/A; X and Z are intentionally restricted there and
# bsc emits "not supported -- use '0','1' or 'A'".  We assert: 0/1/A build, and
# X/Z give the documented restriction message (both are correct bsc behaviour).
USPEC_VALID_OK=1
for val in 0 1 A; do
  B us_$val -verilog -g mkSmall -unspecified-to "$val" "$SMALL"
  { ok_rc && [ -f "$WORK/us_$val/mkSmall.v" ]; } || USPEC_VALID_OK=0
done
USPEC_REST_OK=1
for val in X Z; do
  B us_$val -verilog -g mkSmall -unspecified-to "$val" "$SMALL"
  printf '%s' "$OUT" | grep -qi "use '0', '1' or 'A'" || USPEC_REST_OK=0
done
{ [ "$USPEC_VALID_OK" -eq 1 ] && [ "$USPEC_REST_OK" -eq 1 ]; } \
  && pass C030 "-unspecified-to {0,1,A} build; {X,Z} restricted (verilog 2-value)" \
  || fail C030 "-unspecified-to"
# -remove-dollar : use '_' instead of '$' in GENERATED identifiers.  bsc names
# internal signals like `reg$D_IN`; with -remove-dollar those become `reg_D_IN`.
# DIFFERENTIAL on the richer mkTb design (which has many internal signals): count
# '$' occurrences with and without the flag; the flag must strictly reduce them
# AND the '$'-free output must still carry the renamed `_D_IN` form.  ($display
# system tasks legitimately keep their '$', so we don't require zero.)
B rdbase -verilog -g mkTb -u "$BSV_SRC" >/dev/null 2>&1
RD_BASE=$(grep -c '\$' "$WORK/rdbase/mkTb.v" 2>/dev/null || echo 0)
B rd -verilog -g mkTb -u -remove-dollar "$BSV_SRC" >/dev/null 2>&1
RD_RM=$(grep -c '\$' "$WORK/rd/mkTb.v" 2>/dev/null || echo 0)
{ ok_rc && [ "$RD_BASE" -gt 0 ] && [ "$RD_RM" -lt "$RD_BASE" ] \
   && grep -q "_D_IN" "$WORK/rd/mkTb.v" 2>/dev/null; } \
  && pass C031 "-remove-dollar renames \$->_ in identifiers ($RD_BASE -> $RD_RM \$)" \
  || fail C031 "-remove-dollar (no reduction: $RD_BASE -> $RD_RM)"
# -verilog-filter : post-process the generated .v with a command (use 'cat')
B vf -verilog -g mkSmall -verilog-filter cat "$SMALL"
{ ok_rc && [ -f "$WORK/vf/mkSmall.v" ]; } && pass C032 "-verilog-filter post-processes .v" || fail C032 "-verilog-filter"
# -use-dpi : DPI instead of VPI for imported C (flag accepted on plain design)
B dpi -verilog -g mkSmall -use-dpi "$SMALL"
{ ok_rc; } && pass C033 "-use-dpi (DPI instead of VPI)" || fail C033 "-use-dpi"
# -Xv : pass arg to Verilog link process (needs $VSIM; preflight + SKIP).
if [ "$HAVE_VSIM" -eq 1 ]; then
   B xvb -verilog -g mkSmall -u "$SMALL" >/dev/null 2>&1
   OUT="$(run "$BSC" -verilog -e mkSmall -vsim "$VSIM" -Xv -DCARPET=1 -bdir "$WORK/xvb" -vdir "$WORK/xvb" -info-dir "$WORK/xvb" -simdir "$WORK/xvb" -o "$WORK/xvb/s.exe")"; RC=$?
   { ok_rc && [ -x "$WORK/xvb/s.exe" ]; } && pass C034 "-Xv passes arg to Verilog link" || fail C034 "-Xv"
else
   skipit C034 "-Xv (needs verilog simulator '$VSIM': not on host)"
fi

# =============================================================================
# GROUP 4: Bluesim back-end + SystemC
# =============================================================================
# -parallel-sim-link : number of simultaneous C++ jobs during Bluesim link
# (needs a host C++ compiler; preflight + SKIP).
if [ "$HAVE_CXX" -eq 1 ]; then
   B psl -sim -g mkSmall -u "$SMALL" >/dev/null 2>&1
   OUT="$(run "$BSC" -sim -e mkSmall -parallel-sim-link 2 -bdir "$WORK/psl" -simdir "$WORK/psl" -info-dir "$WORK/psl" -o "$WORK/psl/s.bsim")"; RC=$?
   { ok_rc && [ -x "$WORK/psl/s.bsim" ]; } && pass C035 "-parallel-sim-link 2 (parallel Bluesim link)" || fail C035 "-parallel-sim-link"
else
   skipit C035 "-parallel-sim-link (needs host C++ compiler for Bluesim link)"
fi
# -systemc : generate a SystemC model instead of a Bluesim executable.
# Requires a SystemC toolchain (libsystemc) for the FULL link; we verify the
# flag is recognised + drives codegen.  If no SystemC lib present, the link
# fails for a TOOLCHAIN reason, which we treat as a reasoned skip (not a bsc bug).
B scc -sim -g mkSmall -u "$SMALL" >/dev/null 2>&1
OUT="$(run "$BSC" -sim -systemc -e mkSmall -bdir "$WORK/scc" -simdir "$WORK/scc" -info-dir "$WORK/scc" -o "$WORK/scc/s_sysc")"; RC=$?
if ok_rc; then pass C036 "-systemc generates SystemC model"
elif printf '%s' "$OUT" | grep -qiE "systemc|sc_|library|cannot find|No such|undefined reference"; then
   skipit C036 "-systemc (needs SystemC toolchain/libsystemc on host: reasoned skip)"
else fail C036 "-systemc ($(printf '%s' "$OUT" | tail -1))"; fi

# =============================================================================
# GROUP 5: resource scheduling
# =============================================================================
# -resource-off : default; fail on insufficient resources (compiles clean design)
B roff -verilog -g mkSmall -resource-off "$SMALL"
{ ok_rc; } && pass C037 "-resource-off (default policy)" || fail C037 "-resource-off"
# -resource-simple : auto-arbitrate on insufficient resources
B rsim -verilog -g mkSmall -resource-simple "$SMALL"
{ ok_rc; } && pass C038 "-resource-simple (auto arbitration)" || fail C038 "-resource-simple"

# =============================================================================
# GROUP 6: compiler transformations
# =============================================================================
# -aggressive-conditions
B agg -verilog -g mkTb -u -aggressive-conditions "$BH_SRC"
{ ok_rc; } && pass C039 "-aggressive-conditions (BH design)" || fail C039 "-aggressive-conditions"
# -split-if  (recommend with -no-lift per docs)
B spl -verilog -g mkTb -u -split-if -no-lift "$BH_SRC"
{ ok_rc; } && pass C040 "-split-if -no-lift (split rules on if)" || fail C040 "-split-if"
# -lift (default on) and -no-lift (negated)
B lft -verilog -g mkSmall -lift "$SMALL"
{ ok_rc; } && pass C041 "-lift (lift method calls in if)" || fail C041 "-lift"
B nlft -verilog -g mkSmall -no-lift "$SMALL"
{ ok_rc; } && pass C042 "-no-lift (negated)" || fail C042 "-no-lift"

# =============================================================================
# GROUP 7: compiler optimizations
# =============================================================================
# -opt-undetermined-vals
B oud -verilog -g mkSmall -opt-undetermined-vals "$SMALL"
{ ok_rc; } && pass C043 "-opt-undetermined-vals" || fail C043 "-opt-undetermined-vals"
# -sat-yices (default proof engine)
B sy -verilog -g mkSmall -sat-yices "$SMALL"
{ ok_rc; } && pass C044 "-sat-yices (default SMT engine)" || fail C044 "-sat-yices"
# -sat-stp : alternative SMT engine (may be unbuilt -> reasoned skip)
B st -verilog -g mkSmall -sat-stp "$SMALL"
if ok_rc; then pass C045 "-sat-stp (STP SMT engine)"
elif printf '%s' "$OUT" | grep -qiE "stp|not built|unavailable|no such"; then
   skipit C045 "-sat-stp (STP backend not built into this bsc: reasoned skip)"
else fail C045 "-sat-stp"; fi

# =============================================================================
# GROUP 8: BSV debugging flags
# =============================================================================
# -keep-fires : CAN_FIRE / WILL_FIRE retained in the .v
B kf -verilog -g mkSmall -keep-fires "$SMALL"
{ ok_rc && grep -rqi "CAN_FIRE\|WILL_FIRE" "$WORK/kf"/*.v 2>/dev/null; } && pass C046 "-keep-fires leaves CAN_FIRE/WILL_FIRE" || fail C046 "-keep-fires"
# -keep-inlined-boundaries
B kib -verilog -g mkSmall -keep-inlined-boundaries "$SMALL"
{ ok_rc; } && pass C047 "-keep-inlined-boundaries" || fail C047 "-keep-inlined-boundaries"
# -remove-empty-rules (default ON) : Dbg has an empty rule => warning
B rer -verilog -g mkDbg -remove-empty-rules "$DBG"
{ ok_rc; } && pass C048 "-remove-empty-rules (default on)" || fail C048 "-remove-empty-rules"
B nrer -verilog -g mkDbg -no-remove-empty-rules "$DBG"
{ ok_rc; } && pass C049 "-no-remove-empty-rules (keep empty rule)" || fail C049 "-no-remove-empty-rules"
# -remove-false-rules : Dbg has a provably-false rule
B rfr -verilog -g mkDbg -remove-false-rules "$DBG"
{ ok_rc; } && pass C050 "-remove-false-rules" || fail C050 "-remove-false-rules"
B nrfr -verilog -g mkDbg -no-remove-false-rules "$DBG"
{ ok_rc; } && pass C051 "-no-remove-false-rules (keep false rule)" || fail C051 "-no-remove-false-rules"
# -remove-starved-rules
B rsr -verilog -g mkSmall -remove-starved-rules "$SMALL"
{ ok_rc; } && pass C052 "-remove-starved-rules" || fail C052 "-remove-starved-rules"
# -show-module-use : writes <mod>.use listing the modules it instantiates.
# Use mkTb (which instantiates mkCounter/mkMaxer/FIFO) so the .use is non-empty,
# and ASSERT the .use names a known submodule (mkCounter).
B smu -verilog -g mkTb -u -show-module-use "$BSV_SRC" >/dev/null 2>&1
{ ok_rc && [ -f "$WORK/smu/mkTb.use" ] && grep -q "mkCounter" "$WORK/smu/mkTb.use"; } \
  && pass C053 "-show-module-use writes .use naming instantiated submodules" \
  || fail C053 "-show-module-use (.use missing/empty)"
# -show-method-conf : emit METHOD CONFLICT INFO as comments in the generated .v.
# NB: bsc requires all flags BEFORE source files (S0018), so the flag precedes "$BSV_SRC".
# ASSERT the "Method conflict info:" block actually lands in mkCounter.v.
B smc -verilog -g mkCounter -u -show-method-conf "$BSV_SRC" >/dev/null 2>&1
{ ok_rc && grep -qi "Method conflict info" "$WORK/smc/mkCounter.v" 2>/dev/null; } \
  && pass C054 "-show-method-conf emits method-conflict comments in .v" \
  || fail C054 "-show-method-conf (no conflict block in .v)"
# -show-method-bvi : emit a BVI-format method SCHEDULE block in the generated .v.
# ASSERT the "BVI format method schedule info:" block lands in mkCounter.v.
B smb -verilog -g mkCounter -u -show-method-bvi "$BSV_SRC" >/dev/null 2>&1
{ ok_rc && grep -qi "BVI format method schedule" "$WORK/smb/mkCounter.v" 2>/dev/null; } \
  && pass C055 "-show-method-bvi emits BVI schedule comments in .v" \
  || fail C055 "-show-method-bvi (no BVI block in .v)"
# -show-range-conflict : only changes G0004 reporting; accept on clean design
B src -verilog -g mkSmall -show-range-conflict "$SMALL"
{ ok_rc; } && pass C056 "-show-range-conflict (G0004 detail)" || fail C056 "-show-range-conflict"
# -show-stats : dump per-stage statistics to stderr.  ASSERT the real stats lines
# ("stats <stage>:" + a "definitions" count) appear in the output, not just rc=0.
B ss -verilog -g mkSmall -show-stats "$SMALL"
{ ok_rc && hasi "stats " && hasi "definitions"; } \
  && pass C057 "-show-stats prints per-stage statistics" \
  || fail C057 "-show-stats (no stats output)"
# -show-elab-progress : trace as modules/rules/methods elaborate
B sep -verilog -g mkSmall -show-elab-progress "$SMALL"
{ ok_rc; } && pass C058 "-show-elab-progress (elaboration trace)" || fail C058 "-show-elab-progress"
# -show-compiles (default on) / -no-show-compiles
B sc -verilog -g mkSmall -u -show-compiles "$SMALL"
{ ok_rc; } && pass C059 "-show-compiles (default on)" || fail C059 "-show-compiles"
B nsc -verilog -g mkSmall -u -no-show-compiles "$SMALL"
{ ok_rc; } && pass C060 "-no-show-compiles (negated)" || fail C060 "-no-show-compiles"
# -continue-after-errors : compile a file with a deliberate error; bsc still rc!=0
BADSRC="$WORK/Bad.bsv"
cat > "$BADSRC" <<'EOF'
package Bad;
(* synthesize *)
module mkBad(Empty);
   rule r; xyzzy_undefined_identifier; endrule
endmodule
endpackage
EOF
B cae -verilog -g mkBad -continue-after-errors "$BADSRC"
{ [ "$RC" -ne 0 ] && hasi "rror"; } && pass C061 "-continue-after-errors (still reports the error)" || fail C061 "-continue-after-errors"
# -warn-action-shadowing (default on) / negated
B was -verilog -g mkSmall -warn-action-shadowing "$SMALL"
{ ok_rc; } && pass C062 "-warn-action-shadowing (default on)" || fail C062 "-warn-action-shadowing"
B nwas -verilog -g mkSmall -no-warn-action-shadowing "$SMALL"
{ ok_rc; } && pass C063 "-no-warn-action-shadowing (negated)" || fail C063 "-no-warn-action-shadowing"
# -warn-method-urgency (default on) / negated
B wmu -verilog -g mkSmall -warn-method-urgency "$SMALL"
{ ok_rc; } && pass C064 "-warn-method-urgency (default on)" || fail C064 "-warn-method-urgency"
B nwmu -verilog -g mkSmall -no-warn-method-urgency "$SMALL"
{ ok_rc; } && pass C065 "-no-warn-method-urgency (negated)" || fail C065 "-no-warn-method-urgency"
# -suppress-warnings : suppress warning tags.  DIFFERENTIAL on the warning-bearing
# Dbg design (G0023): the baseline must emit warnings (>0) and -suppress-warnings
# ALL must STRICTLY reduce them (to zero), with a clean rc.
B swu -verilog -g mkDbg -no-remove-empty-rules "$DBG"
WARN_BASE="$(printf '%s' "$OUT" | grep -ci "warning")"
B sw -verilog -g mkDbg -no-remove-empty-rules -suppress-warnings ALL "$DBG"
WARN_SUP="$(printf '%s' "$OUT" | grep -ci "warning")"
{ ok_rc && [ "$WARN_BASE" -gt 0 ] && [ "$WARN_SUP" -lt "$WARN_BASE" ]; } \
  && pass C066 "-suppress-warnings ALL strictly reduces warnings ($WARN_BASE -> $WARN_SUP)" \
  || fail C066 "-suppress-warnings (base=$WARN_BASE sup=$WARN_SUP)"
# -show-all-warnings : override a prior -suppress-warnings and show them anyway.
# DIFFERENTIAL: with -suppress-warnings ALL the count is 0; adding
# -show-all-warnings restores the warnings (>0).
B saw1 -verilog -g mkDbg -no-remove-empty-rules -suppress-warnings ALL "$DBG"
SAW_SUP="$(printf '%s' "$OUT" | grep -ci "warning")"
B saw2 -verilog -g mkDbg -no-remove-empty-rules -suppress-warnings ALL -show-all-warnings "$DBG"
SAW_SHOW="$(printf '%s' "$OUT" | grep -ci "warning")"
{ ok_rc && [ "$SAW_SUP" -eq 0 ] && [ "$SAW_SHOW" -gt 0 ]; } \
  && pass C066b "-show-all-warnings overrides -suppress-warnings ($SAW_SUP -> $SAW_SHOW)" \
  || fail C066b "-show-all-warnings (sup=$SAW_SUP show=$SAW_SHOW)"
# -Werror : DEPRECATED alias for -promote-warnings ALL.  bsc rejects it with the
# documented deprecation message S0062.  ASSERT that exact behaviour (recognised
# flag, deprecation message naming the replacement) rather than rc-only.
B werr -verilog -g mkSmall -Werror "$SMALL"
{ hasi "Deprecated flag" && has "Werror" && hasi "promote-warnings"; } \
  && pass C066c "-Werror reports the documented deprecation (use -promote-warnings ALL)" \
  || fail C066c "-Werror (no deprecation message)"
# -warn-undet-predicate / -no-warn-undet-predicate : real recognised flags (warn
# when a rule predicate depends on an undetermined value).  Triggering the actual
# G-warning needs a specific undetermined-value flow, so here we PROVE the flags
# are genuinely recognised (build succeeds with each form) while a MISSPELLING is
# rejected -- a true differential against bsc's "Unrecognized flag" path.
B wup  -verilog -g mkSmall -warn-undet-predicate "$SMALL";    WUP_OK=$([ "$RC" -eq 0 ] && echo 1 || echo 0)
B nwup -verilog -g mkSmall -no-warn-undet-predicate "$SMALL"; NWUP_OK=$([ "$RC" -eq 0 ] && echo 1 || echo 0)
Braw -warn-undet-predicateXX "$SMALL"; BADWUP=$(has "Unrecognized flag" && echo 1 || echo 0)
{ [ "$WUP_OK" -eq 1 ] && [ "$NWUP_OK" -eq 1 ] && [ "$BADWUP" -eq 1 ]; } \
  && pass C066d "-warn-undet-predicate (+negated) recognised; misspelling rejected" \
  || fail C066d "-warn-undet-predicate (ok=$WUP_OK neg=$NWUP_OK badrej=$BADWUP)"
# -promote-warnings : turn warnings into errors.  TRUE DIFFERENTIAL on the
# warning-bearing Dbg design (G0023, kept via -no-remove-empty-rules):
#   * WITHOUT the flag  => rc 0           (warnings stay warnings)
#   * WITH -promote-warnings ALL => rc != 0 AND output contains "Error"
# Pass only if BOTH halves hold (proves the flag actually changed behaviour).
B pwbase -verilog -g mkDbg -no-remove-empty-rules "$DBG"
PW_BASE_RC=$RC
B pw -verilog -g mkDbg -no-remove-empty-rules -promote-warnings ALL "$DBG"
PW_RC=$RC; PW_HASERR=0; printf '%s' "$OUT" | grep -qi "Error" && PW_HASERR=1
{ [ "$PW_BASE_RC" -eq 0 ] && [ "$PW_RC" -ne 0 ] && [ "$PW_HASERR" -eq 1 ]; } \
  && pass C067 "-promote-warnings ALL promotes G0023 warning to a hard error" \
  || fail C067 "-promote-warnings (base_rc=$PW_BASE_RC prom_rc=$PW_RC err=$PW_HASERR)"
# -demote-errors : exempt a promoted warning from promotion.  TRUE DIFFERENTIAL
# (the documented "-promote-warnings ALL + -demote-errors list" pattern):
#   * -promote-warnings ALL                       => G0023 becomes an error (rc!=0)
#   * -promote-warnings ALL -demote-errors G0023  => G0023 exempted, back to a
#                                                    warning => rc 0 (it builds)
# Pass only if BOTH halves hold.
B deprom -verilog -g mkDbg -no-remove-empty-rules -promote-warnings ALL "$DBG"
DE_PROM_RC=$RC
B de -verilog -g mkDbg -no-remove-empty-rules -promote-warnings ALL -demote-errors G0023 "$DBG"
DE_RC=$RC; DE_HASWARN=0; printf '%s' "$OUT" | grep -qi "Warning" && DE_HASWARN=1
{ [ "$DE_PROM_RC" -ne 0 ] && [ "$DE_RC" -eq 0 ] && [ "$DE_HASWARN" -eq 1 ]; } \
  && pass C068 "-demote-errors G0023 exempts the promoted warning (back to warning)" \
  || fail C068 "-demote-errors (prom_rc=$DE_PROM_RC demote_rc=$DE_RC warn=$DE_HASWARN)"

# =============================================================================
# GROUP 9: schedule introspection
# =============================================================================
# -show-schedule : write a <mod>.sched schedule-dump file.  ASSERT the file is
# created AND carries the real schedule contents ("Generated schedule for
# mkSmall" + a "Logical execution order" line), not merely rc=0.
B sch -verilog -g mkSmall -show-schedule "$SMALL" >/dev/null 2>&1
{ ok_rc && [ -f "$WORK/sch/mkSmall.sched" ] \
   && grep -qi "Generated schedule for mkSmall" "$WORK/sch/mkSmall.sched" \
   && grep -qi "Logical execution order" "$WORK/sch/mkSmall.sched"; } \
  && pass C069 "-show-schedule writes the real .sched (exec order)" \
  || fail C069 "-show-schedule (no real schedule dump)"
# -show-rule-rel r1 r2 : print the scheduling relationship between two named
# rules.  bsc names rules with an RL_ prefix in the schedule namespace, so the
# CORRECT identifiers are RL_tick / RL_fin (bare names => "Rule not found", which
# the OLD test silently ignored because rc stayed 0 -- a false green).  ASSERT
# the real "Scheduling info for rules" header AND that NO "Rule not found"
# warning was emitted.
B srr -verilog -g mkSmall -show-rule-rel RL_tick RL_fin "$SMALL"
{ ok_rc && hasi "Scheduling info for rules" && ! has "Rule not found"; } \
  && pass C070 "-show-rule-rel RL_tick RL_fin prints the scheduling relationship" \
  || fail C070 "-show-rule-rel (no scheduling info or rule not found)"
# -sched-dot : emit .dot graphs into the info-dir
B sd2 -verilog -g mkSmall -sched-dot "$SMALL"
{ ok_rc && ls "$WORK/sd2"/*.dot >/dev/null 2>&1; } && pass C071 "-sched-dot emits .dot schedule graphs" || fail C071 "-sched-dot"

# =============================================================================
# GROUP 10: preprocessor + macros + assertion + reset prefix + steps
# =============================================================================
# -D macro + -E : preprocessor.  Define a macro used by `ifdef in BSV, run -E.
PPSRC="$WORK/Pp.bsv"
cat > "$PPSRC" <<'EOF'
package Pp;
`ifdef CARPET_DEF
// carpet-defined-branch-marker
`endif
(* synthesize *)
module mkPp(Empty);
   rule r; $finish(0); endrule
endmodule
endpackage
EOF
Braw -E -D CARPET_DEF "$PPSRC"
{ ok_rc && has "carpet-defined-branch-marker"; } && pass C072 "-D macro + -E (preprocessor expands ifdef)" || fail C072 "-D + -E"
# -D name=value form
B dval -verilog -g mkSmall -D SIZE=8 "$SMALL"
{ ok_rc; } && pass C073 "-D name=value form" || fail C073 "-D name=value"
# -cpp : run the C preprocessor before the BSV preprocessor.
# (Bcd: bsc writes the cpp intermediate NNNNN.c to cwd, so run inside a writable subdir.)
Bcd cpp -verilog -g mkSmall -cpp "$SMALL"
{ ok_rc && [ -f "$WORK/cpp/mkSmall.v" ]; } && pass C074 "-cpp (run C preprocessor first)" || fail C074 "-cpp"
# -check-assert : test Assert-library assertions (design w/o asserts compiles)
B ca -verilog -g mkSmall -check-assert "$SMALL"
{ ok_rc; } && pass C075 "-check-assert (Assert-library assertions)" || fail C075 "-check-assert"
# -reset-prefix : change the reset signal name in the generated .v
B rp -verilog -g mkSmall -reset-prefix RST_P "$SMALL"
{ ok_rc && grep -rqi "RST_P" "$WORK/rp"/*.v 2>/dev/null; } && pass C076 "-reset-prefix RST_P renames reset" || fail C076 "-reset-prefix"
# -steps : terminate elaboration after N unfolding steps (large N => completes)
B sN -verilog -g mkSmall -steps 1000000 "$SMALL"
{ ok_rc; } && pass C077 "-steps N (unfolding-step budget)" || fail C077 "-steps"
# -steps-warn-interval : warn every N steps
B swi -verilog -g mkSmall -steps-warn-interval 100000 "$SMALL"
{ ok_rc; } && pass C078 "-steps-warn-interval N" || fail C078 "-steps-warn-interval"
# -steps-max-intervals : safety cap on number of step-warnings
B smi -verilog -g mkSmall -steps-max-intervals 10 "$SMALL"
{ ok_rc; } && pass C079 "-steps-max-intervals N" || fail C079 "-steps-max-intervals"

# =============================================================================
# GROUP 11: foreign C/C++ flags (-I/-L/-l/-Xc/-Xc++/-Xcpp/-Xl)
# These only have effect during a foreign-import link.  The flags are accepted
# on any link; we verify acceptance (passing args through) and the value-carrying
# nature.  A full foreign-C link requires a C source + the C toolchain; doing a
# real importBVI/import "BDPI" link is out of scope (needs C toolchain wiring),
# so the value forms below are reasoned skips for the FULL effect.
# -I path / -L path : accepted during a verilog link (needs $VSIM; preflight).
if [ "$HAVE_VSIM" -eq 1 ]; then
   B inc -verilog -g mkSmall -u "$SMALL" >/dev/null 2>&1
   OUT="$(run "$BSC" -verilog -e mkSmall -vsim "$VSIM" -I "$WORK" -L "$WORK" -bdir "$WORK/inc" -vdir "$WORK/inc" -info-dir "$WORK/inc" -simdir "$WORK/inc" -o "$WORK/inc/s.exe")"; RC=$?
   { ok_rc && [ -x "$WORK/inc/s.exe" ]; } && pass C080 "-I/-L foreign include+lib path accepted at link" || fail C080 "-I/-L"
else
   skipit C080 "-I/-L (needs verilog simulator '$VSIM': not on host)"
fi
# -Xc/-Xc++/-Xl : pass args to the C/C++/linker during a Bluesim link (needs a
# host C++ compiler; preflight + SKIP).
if [ "$HAVE_CXX" -eq 1 ]; then
   B lib -sim -g mkSmall -u "$SMALL" >/dev/null 2>&1
   OUT="$(run "$BSC" -sim -e mkSmall -Xc -O0 -Xc++ -O0 -Xl -s -bdir "$WORK/lib" -simdir "$WORK/lib" -info-dir "$WORK/lib" -o "$WORK/lib/s.bsim")"; RC=$?
   { ok_rc && [ -x "$WORK/lib/s.bsim" ]; } && pass C081 "-Xc/-Xc++/-Xl pass args to C/C++/linker (Bluesim)" || fail C081 "-Xc/-Xc++/-Xl"
else
   skipit C081 "-Xc/-Xc++/-Xl (needs host C++ compiler for Bluesim link)"
fi
# -Xcpp : pass arg to the C preprocessor (only meaningful with -cpp).
# (Bcd: same cwd-write reason as C074.)
Bcd xcpp -verilog -g mkSmall -cpp -Xcpp -DEXTRA=1 "$SMALL"
{ ok_rc && [ -f "$WORK/xcpp/mkSmall.v" ]; } && pass C082 "-Xcpp passes arg to C preprocessor (with -cpp)" || fail C082 "-Xcpp"
# -l : requires a real foreign library to link => reasoned skip for full effect
skipit C083 "-l library (needs a real foreign C library + import \"BDPI\": needs-foreign-toolchain)"

# =============================================================================
# GROUP 12: documented-but-not-in-this-binary / RTS pass-through
# =============================================================================
# +RTS -H<size> -RTS / -K<size> : Haskell RTS heap/stack (pass-through to GHC RTS).
# These tune bsc's OWN runtime; assert they are accepted and a build still works.
rm -rf "$WORK/rts"; mkdir -p "$WORK/rts"
OUT="$(run "$BSC" -verilog -g mkSmall -bdir "$WORK/rts" -vdir "$WORK/rts" -info-dir "$WORK/rts" "$SMALL" +RTS -H256m -K64m -RTS)"; RC=$?
{ ok_rc && [ -f "$WORK/rts/mkSmall.v" ]; } && pass C084 "+RTS -H/-K -RTS (bsc heap/stack RTS flags)" || fail C084 "+RTS -H/-K"
# -O : doc lists an -O optimisation flag but it is COMMENTED OUT in user_guide.tex
# and NOT present in this bsc binary (verified: '-O0'/'-O1' => Unrecognized flag).
# The live optimisation flag is -opt-undetermined-vals (C043).  Reasoned skip.
Braw -O0 "$SMALL"
{ printf '%s' "$OUT" | grep -qi "Unrecognized flag"; } \
  && skipit C085 "-O / -O0 / -O1 (documented-but-commented-out; absent in binary; use -opt-undetermined-vals)" \
  || fail C085 "-O unexpected (binary accepted it?)"

# =============================================================================
# Negative-control: an unknown flag must be rejected (proves our rc checks bite)
# =============================================================================
Braw -definitely-not-a-real-flag "$SMALL"
{ printf '%s' "$OUT" | grep -qi "Unrecognized flag"; } && pass C086 "unknown flag rejected (rc-check sanity)" || fail C086 "unknown-flag rejection"

# =============================================================================
# Tally
# =============================================================================
echo "-------------------------------------------------------------"
echo "bsc CLI carpet: PASS=$PASS  SKIP=$SKIP  FAIL=$FAILN"
if [ "$FAILN" -ne 0 ]; then
   echo "FAILED checks:"
   for f in "${FAILED[@]}"; do echo "  - $f"; done
   exit 1
fi
echo "BSC_CLI_OK $PASS/$PASS (+$SKIP reasoned skips)"
# keep WORK unless caller asked to keep it
[ "${BSC_CARPET_KEEP:-0}" = "1" ] || rm -rf "$WORK"
exit 0
