#!/usr/bin/env bash
# prebuild.sh — provision the HDL (Verilog/SystemVerilog + Bluespec + GNU Make +
# yosys) carpet for StarryOS, MODEL A (static binaries, like go-lang / the merged
# verilog & bluesv hw4os cases).
#
# Six host-compiled simulations/tools are staged as STATIC on-target artifacts; on
# StarryOS each runs and is `cmp`d byte-for-byte against the host-captured golden.
# The aggregate is TEST PASSED only if ALL of VLOG / IVL / BSV / BH / MAKE / yosys
# pass.
#
#   VLOG  — Verilator 5.008 verilates the comprehensive SV design (rtl/*.sv +
#           tb/tb_top.sv) to C++, cross-compiled with musl-cross g++ to a fully
#           static `hdl-tb-vlt` for STARRY_ARCH (no libc/interp). 139 self-checks.
#   IVL   — Icarus Verilog 12 compiles the SAME SV design to a portable bytecode
#           `tb_ivl.vvp`, run on-target by a STATIC, musl-cross-built `vvp`
#           runtime (system VPI + VCD embedded). MUST match VLOG byte-for-byte.
#   BSV   — bsc 2026.01 compiles LangBSV.bsv -> Verilog -> a `LangBSV.vvp` (bsc
#           `-vsim iverilog` output is itself an Icarus bytecode file), run by the
#           same static vvp.
#   BH    — bsc compiles LangBH.bs (Bluespec Classic / Haskell) the same way.
#   MAKE  — a self-contained GNU Make language-feature Makefile (variables :=/?=/+=,
#           functions, pattern rules, automatic vars, .PHONY, conditionals,
#           include), run on-target by a STATIC, musl-cross-built GNU Make 4.4.1
#           `lang-make`. Output must EXACT-match the host golden.
#   yosys — yosys 0.58 runs the full generic-synthesis flow (proc/opt/fsm/memory/
#           techmap) on a comprehensive RTL design (alu+ctrl-FSM+datapath-RAM) to
#           a gate-level netlist; a self-checking testbench drives the SYNTHESIZED
#           netlist (+ yosys simlib) into `yosys_net.vvp`, run by the static vvp.
#           yosys itself is a host-only synthesizer (no on-target binary); this
#           leg verifies the synthesis RESULT is functionally correct on-target.
#
# The static `vvp` runtime cannot dlopen() the usual system.vpi, so each bytecode's
# `:vpi_module "<abs>/system.vpi"` line is rewritten to `:vpi_module "system"` and
# the other vpi_module lines (vhdl/math, host abs-paths) are dropped — the system
# module ($display/$finish/$dumpvars/...) is linked into the runtime directly.
#
# Cross binaries are run on the build host through qemu-<arch>-static for golden
# capture / self-test, so this prebuild works for ANY STARRY_ARCH on ANY host.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR,
# STARRY_STAGING_ROOT (scratch).
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
work="${STARRY_STAGING_ROOT:-/tmp}/hdl-lang-build-$arch"

# Host toolchain (overridable). Versions used: verilator 5.008, iverilog/vvp 12,
# bsc 2026.01 at /usr/local/bsc, yosys 0.58, GNU make 4.x.
VERILATOR="${VERILATOR:-verilator}"
IVERILOG="${IVERILOG:-iverilog}"
YOSYS="${YOSYS:-yosys}"
BSC="${BSC:-/usr/local/bsc/bin/bsc}"
MAKE="${MAKE:-make}"
export BLUESPECDIR="${BLUESPECDIR:-/usr/local/bsc/lib}"

# Preflight: require host tools (consistent with the musl-cross/qemu-static preflight below)
command -v "$VERILATOR" >/dev/null 2>&1 || { echo "prebuild: verilator not found ($VERILATOR)" >&2; exit 1; }
command -v "$IVERILOG"  >/dev/null 2>&1 || { echo "prebuild: iverilog not found ($IVERILOG)" >&2; exit 1; }
command -v "$YOSYS"     >/dev/null 2>&1 || { echo "prebuild: yosys not found ($YOSYS)" >&2; exit 1; }
command -v "$MAKE"      >/dev/null 2>&1 || { echo "prebuild: make not found ($MAKE)" >&2; exit 1; }
[[ -x "$BSC" ]]                     || { echo "prebuild: bsc not found at $BSC" >&2; exit 1; }

case "$arch" in
    x86_64|aarch64|riscv64|loongarch64) : ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# Runner: execute a STARRY_ARCH binary on the build host. Native if the build host
# already is this arch, else through qemu-<arch>-static (must be installed).
host_arch="$(uname -m)"
if [[ "$host_arch" == "$arch" ]]; then
    runner() { "$@"; }
else
    QEMU_STATIC="${QEMU_STATIC:-qemu-$arch-static}"
    command -v "$QEMU_STATIC" >/dev/null 2>&1 \
        || { echo "prebuild: need $QEMU_STATIC to run $arch binaries on $host_arch host" >&2; exit 1; }
    runner() { "$QEMU_STATIC" "$@"; }
fi

# musl-cross g++ for the verilator static sim (matches the hw4os hdl cases).
musl_cross=""
for c in "/opt/$arch-linux-musl-cross/bin/$arch-linux-musl-g++" \
         "/usr/local/$arch-linux-musl-cross/bin/$arch-linux-musl-g++"; do
    [[ -x "$c" ]] && { musl_cross="$c"; break; }
done
[[ -n "$musl_cross" ]] || { echo "prebuild: missing musl-cross g++ for $arch" >&2; exit 1; }

# Pre-built STATIC runtimes shipped alongside this case (cannot be rebuilt from
# this dir alone): the VCD-enabled vvp (system VPI embedded) and GNU Make.
vvp_runtime="$app_dir/vendor/vvp/vvp-$arch"
make_runtime="$app_dir/vendor/make/make-$arch"
[[ -x "$vvp_runtime"  ]] || { echo "prebuild: missing static vvp runtime $vvp_runtime" >&2; exit 1; }
[[ -x "$make_runtime" ]] || { echo "prebuild: missing static make runtime $make_runtime" >&2; exit 1; }

vroot="$($VERILATOR --getenv VERILATOR_ROOT 2>/dev/null)"
[[ -d "$vroot/include" ]] || { echo "prebuild: cannot locate VERILATOR_ROOT include dir" >&2; exit 1; }

rm -rf "$work"; mkdir -p "$work"
mkdir -p "$overlay_dir/usr/local/bin" "$overlay_dir/root"

echo "=== HDL language carpet prebuild — arch=$arch (host=$host_arch) ==="
# Cosmetic tool banner; never let a SIGPIPE from `head` abort under pipefail.
{ $VERILATOR --version 2>&1 || true; } | sed -n '1p' || true
{ $IVERILOG  -V 2>&1 || true; } | sed -n '1p' || true
{ $BSC -v </dev/null 2>&1 || true; } | sed -n '1p' || true
{ $YOSYS -V 2>&1 || true; } | sed -n '1p' || true
{ $MAKE --version 2>&1 || true; } | sed -n '1p' || true

# Rewrite an Icarus .vvp bytecode file so the STATIC vvp (embedded system module,
# no dlopen) can load it: keep one `:vpi_module "system"`, drop the rest.
clean_vvp() {
    local f="$1"
    sed -i 's#:vpi_module "[^"]*system\.vpi"#:vpi_module "system"#' "$f"
    sed -i '/:vpi_module "system";/!{/:vpi_module /d}' "$f"
}

# ---------------------------------------------------------------------------
# Ordered, compile-correct SV source list (package first).
# ---------------------------------------------------------------------------
rtl="$app_dir/src/rtl"; tb="$app_dir/src/tb"
ORDERED="$rtl/hdl_pkg.sv $rtl/alu.sv $rtl/regfile.sv $rtl/counter.sv \
$rtl/fsm_traffic.sv $rtl/fsm_seqdet.sv $rtl/shifter.sv $rtl/genblk.sv \
$rtl/latch.sv $tb/tb_top.sv"
VLT_WAIVERS="-Wno-WIDTH -Wno-CASEX -Wno-CASEINCOMPLETE -Wno-TIMESCALEMOD \
-Wno-LITENDIAN -Wno-UNOPTFLAT -Wno-LATCH"

# ===========================================================================
# (1) VLOG — Verilator -> C++ -> static musl cross binary `hdl-tb-vlt`.
# ===========================================================================
echo "prebuild: [VLOG] verilating SV design -> C++"
vobj="$work/vobj"; mkdir -p "$vobj"
$VERILATOR --cc --main --timing --top-module tb_top $VLT_WAIVERS \
    $ORDERED --Mdir "$vobj" --prefix Vtb_top > "$work/vlt_cc.log" 2>&1
echo "prebuild: [VLOG] cross-compiling static $arch sim via $(basename "$musl_cross")"
# riscv64/loongarch64 musl-cross GCC 11/13 emit dynamic relocations into RELRO
# under static-PIE -> `read-only segment has dynamic relocations`; build no-pie
# (same as the hw4os hdl cases). x86_64/aarch64 link clean as static-PIE.
pie_flags=()
case "$arch" in riscv64|loongarch64) pie_flags=(-fno-pie -no-pie) ;; esac
# Compile each translation unit separately and in parallel, then link. A single
# monolithic g++ invocation peaks too much RAM / wall-clock with the slower
# loongarch/riscv GCCs (OOM-kill / timeout); per-TU keeps peak memory low.
objs=()
for cpp in "$vobj"/Vtb_top*.cpp \
           "$vroot/include/verilated.cpp" \
           "$vroot/include/verilated_threads.cpp" \
           "$vroot/include/verilated_timing.cpp"; do
    o="$vobj/$(basename "${cpp%.cpp}").o"
    objs+=("$o")
    "$musl_cross" -O2 -std=gnu++20 "${pie_flags[@]}" \
        -I"$vobj" -I"$vroot/include" -I"$vroot/include/vltstd" \
        -c "$cpp" -o "$o" >> "$work/vlt_cc_obj.log" 2>&1 &
done
wait
"$musl_cross" "${pie_flags[@]}" \
    -static -static-libgcc -static-libstdc++ -pthread \
    "${objs[@]}" -o "$overlay_dir/usr/local/bin/hdl-tb-vlt" > "$work/vlt_link.log" 2>&1
# Capture the golden from the HOST-NATIVE verilator binary: the SV testbench is
# fully deterministic, so its TB: output is IDENTICAL on every arch. This avoids
# depending on qemu-user, whose C++20-coroutine emulation can stall on some
# arches (e.g. loongarch). The cross binary is then self-tested best-effort below.
echo "prebuild: [VLOG] capturing arch-independent host golden"
hvlt="$work/host_vlt"; rm -rf "$hvlt"; mkdir -p "$hvlt"
$VERILATOR --binary --timing --top-module tb_top $VLT_WAIVERS \
    --Mdir "$hvlt" -o tb_vlt $ORDERED > "$work/vlt_host.log" 2>&1
"$hvlt/tb_vlt" 2>/dev/null | grep '^TB:' > "$work/vlog.txt"
grep -q '^TB: CARPET_RESULT ALL_PASS$' "$work/vlog.txt" \
    || { echo "prebuild: [VLOG] host verilator sim did not ALL_PASS" >&2; exit 2; }
install -Dm0644 "$work/vlog.txt" "$overlay_dir/root/hdl-vlog-golden.txt"
# Best-effort self-test of the cross binary on the build host (skipped if the
# emulator stalls; the authoritative run is on real StarryOS via qemu-<arch>.toml).
if timeout 120 bash -c 'runner() { '"$([[ "$host_arch" == "$arch" ]] && echo '"$@"' || echo "$QEMU_STATIC"' "$@"')"'; }; runner "$1" 2>/dev/null | grep "^TB:" > "$2"' _ "$overlay_dir/usr/local/bin/hdl-tb-vlt" "$work/vlog_cross.txt" 2>/dev/null \
   && [[ -s "$work/vlog_cross.txt" ]]; then
    cmp -s "$work/vlog_cross.txt" "$work/vlog.txt" \
        && echo "prebuild: [VLOG] cross sim self-test == golden (on host)" \
        || { echo "prebuild: [VLOG] cross sim self-test != golden" >&2; diff "$work/vlog.txt" "$work/vlog_cross.txt" | head; exit 2; }
else
    echo "prebuild: [VLOG] cross sim host self-test skipped (emulator stall / arch); on-target run is authoritative"
fi

# ===========================================================================
# (2) IVL — Icarus -> portable .vvp, run by the static vvp. Must == VLOG.
# ===========================================================================
echo "prebuild: [IVL] building portable tb_ivl.vvp"
$IVERILOG -g2012 -gassertions -o "$work/tb_ivl.vvp" $ORDERED 2> "$work/ivl_build.log"
clean_vvp "$work/tb_ivl.vvp"
runner "$vvp_runtime" "$work/tb_ivl.vvp" 2>/dev/null | grep '^TB:' > "$work/ivl.txt"
cmp -s "$work/vlog.txt" "$work/ivl.txt" \
    || { echo "prebuild: [IVL] iverilog output != verilator golden" >&2; diff "$work/vlog.txt" "$work/ivl.txt" | head; exit 2; }
install -Dm0644 "$work/tb_ivl.vvp" "$overlay_dir/root/hdl-tb-ivl.vvp"

# ===========================================================================
# (3)/(4) BSV/BH — bsc <design> -> Verilog -> .vvp.
# ===========================================================================
build_bsv() {  # <name> <src> <top> <golden-name> <vvp-name> <sentinel>
    local name="$1" src="$2" top="$3" gname="$4" vname="$5" sentinel="$6"
    local d="$work/$name"; mkdir -p "$d"
    echo "prebuild: [$name] bsc compile $top"
    ( cd "$d" && $BSC -verilog -u -g "$top" -vdir "$d" -bdir "$d" -info-dir "$d" "$src" \
        > "$work/${name}_compile.log" 2>&1 )
    ( cd "$d" && $BSC -verilog -e "$top" -vsim iverilog -o "$d/sim" -vdir "$d" -bdir "$d" "$d/$top.v" \
        > "$work/${name}_link.log" 2>&1 )
    # bsc's `-vsim iverilog` output IS an Icarus .vvp; clean it for the static vvp.
    cp "$d/sim" "$work/$vname"
    clean_vvp "$work/$vname"
    runner "$vvp_runtime" "$work/$vname" 2>/dev/null > "$work/$name.txt"
    grep -q "^${sentinel}\$" "$work/$name.txt" \
        || { echo "prebuild: [$name] missing $sentinel sentinel" >&2; tail "$work/$name.txt"; exit 2; }
    # Bluespec fshow() renders tagged-union values with a trailing space; normalize it
    # away so the golden has no trailing whitespace (git diff --check hygiene). run-hdl.sh
    # applies the identical normalization to the on-target output, so cmp stays byte-exact.
    sed -i 's/[[:space:]]*$//' "$work/$name.txt"
    install -Dm0644 "$work/$name.txt" "$overlay_dir/root/$gname"
    install -Dm0644 "$work/$vname"    "$overlay_dir/root/$vname"
}
build_bsv bsv "$app_dir/src/LangBSV.bsv" mkLangBSV hdl-bsv-golden.txt LangBSV.vvp BSV_DONE
build_bsv bh  "$app_dir/src/LangBH.bs"   mkLangBH  hdl-bh-golden.txt  LangBH.vvp  BH_DONE

# ===========================================================================
# (5) MAKE — self-contained GNU Make language carpet, run by the static make.
# ===========================================================================
echo "prebuild: [MAKE] staging Makefile + static make + capturing golden"
mkdir -p "$overlay_dir/root/make"
install -Dm0644 "$app_dir/src/make/Makefile"  "$overlay_dir/root/make/Makefile"
install -Dm0644 "$app_dir/src/make/config.mk" "$overlay_dir/root/make/config.mk"
# Capture the golden with the SAME static make (run via the runner) so host/target
# agree exactly. Run inside the staged dir so `include config.mk` resolves.
( cd "$overlay_dir/root/make" && runner "$make_runtime" -s -f Makefile ) > "$work/make.txt" 2>&1
grep -q '^MAKE_LANG_OK$' "$work/make.txt" \
    || { echo "prebuild: [MAKE] missing MAKE_LANG_OK token" >&2; cat "$work/make.txt"; exit 2; }
install -Dm0644 "$work/make.txt" "$overlay_dir/root/hdl-make-golden.txt"
install -Dm0755 "$make_runtime"  "$overlay_dir/usr/local/bin/lang-make"

# ===========================================================================
# (6) yosys — full synthesis flow -> gate netlist -> simulated on-target.
# ===========================================================================
echo "prebuild: [yosys] running generic-synthesis flow -> gate netlist"
ys="$app_dir/src/yosys"; yd="$work/ys"; mkdir -p "$yd"
$YOSYS -Q -T -p "read_verilog $ys/alu.v $ys/ctrl.v $ys/datapath.v; \
hierarchy -check -top datapath; proc; opt; fsm; memory; opt; techmap; opt; \
write_verilog -noattr $yd/net.v" > "$work/ys_synth.log" 2>&1
# yosys simulation library (simcells.v/simlib.v) for the techmapped netlist.
simdir="$(yosys-config --datdir 2>/dev/null || true)"
[[ -f "$simdir/simcells.v" ]] || simdir="$(dirname "$(command -v "$YOSYS")")/../share/yosys"
[[ -f "$simdir/simcells.v" ]] || { echo "prebuild: [yosys] cannot locate simcells.v" >&2; exit 1; }
echo "prebuild: [yosys] building post-synthesis netlist sim"
$IVERILOG -g2012 -o "$work/yosys_net.vvp" \
    "$yd/net.v" "$ys/tb_synth.v" "$simdir/simcells.v" "$simdir/simlib.v" \
    > "$work/ys_ivl.log" 2>&1
clean_vvp "$work/yosys_net.vvp"
runner "$vvp_runtime" "$work/yosys_net.vvp" 2>/dev/null | grep '^SYN' > "$work/ys.txt"
grep -q '^SYN_DONE$' "$work/ys.txt" \
    || { echo "prebuild: [yosys] missing SYN_DONE sentinel" >&2; exit 2; }
install -Dm0644 "$work/yosys_net.vvp" "$overlay_dir/root/yosys_net.vvp"
install -Dm0644 "$work/ys.txt"        "$overlay_dir/root/hdl-yosys-golden.txt"

# ===========================================================================
# Stage the single static vvp runtime that drives IVL / BSV / BH / yosys.
# ===========================================================================
install -Dm0755 "$vvp_runtime" "$overlay_dir/usr/local/bin/vvp"

# Stage the on-target gate script (invoked as the ENTIRE shell_init_cmd). Keeping the gate
# in a staged script — not inline in the toml — avoids the harness false-positive where the
# echoed shell_init_cmd text containing `echo "TEST PASSED"` would self-match success_regex.
install -Dm0755 "$app_dir/src/run-hdl.sh" "$overlay_dir/usr/local/bin/run-hdl.sh"

echo "prebuild: hdl-lang ready for $arch — VLOG $(wc -l <"$work/vlog.txt")L / IVL $(wc -l <"$work/ivl.txt")L / BSV $(wc -l <"$work/bsv.txt")L / BH $(wc -l <"$work/bh.txt")L / MAKE $(wc -l <"$work/make.txt")L / yosys $(wc -l <"$work/ys.txt")L"
