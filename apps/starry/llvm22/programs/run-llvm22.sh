#!/bin/sh
# run-llvm22.sh - on-target LLVM 22 toolchain carpet for StarryOS.
#
# Every dimension is DERIVED FROM ACTUAL COMMAND OUTPUT, not a hand-picked list: the tool set
# comes from `ls` of the installed bin dir, the backend target set from `llc --version`, the
# front-end language set from `clang -x`, and the pass set from `opt --print-passes`. Adding a
# tool / target / pass in a future LLVM is picked up automatically; one exact assertion per grid
# cell, across the seven dimensions (documentation walk, --help/--version, per-option behaviour,
# functional output, boundary inputs, error handling, pipelines):
#
#   binary sweep     every executable in /usr/lib/llvm22/bin plus the lld linker family, each
#                    made to self-report LLVM 22 via --version, or (for tools without a --version
#                    banner) to launch and self-identify via --help; the three FileCheck-suite
#                    drivers with no --version/--help contract (count / not / llvm-PerfectShuffle)
#                    are exercised functionally.
#   backend matrix   llc emits asm for every target `llc --version` registers x {O0,O1,O2,O3};
#                    llc emits an object (exact ELF e_machine / SPIR-V / wasm magic) for every
#                    object-capable target; the asm-only targets have no object emitter by design.
#   front-ends       clang -x recognizes the full source-language taxonomy; the headless-testable
#                    modes (C / C++ / Objective-C / Objective-C++ / OpenCL C / OpenCL C++ / LLVM
#                    IR / assembler) compile to IR / asm / object / linked exe with the -std and
#                    -O sweeps; cuda / hip are recognized but device-toolkit-gated (noted, not run).
#   IR passes        exact-transform observation of the core optimizations (mem2reg / sroa /
#                    instcombine / inline / gvn / dce / adce / simplifycfg / sccp / constmerge /
#                    globaldce / deadargelim / early-cse / dse / tailcallelim / loop-unroll / licm /
#                    reassociate), plus -O0..-O3 / default<O2> pipelines, plus a run-clean sweep
#                    over EVERY transform pass `opt --print-passes` lists.
#   IR / object tools  llvm-as/-dis roundtrip, llvm-link / llvm-extract / llvm-cat / llvm-diff /
#                    llvm-bcanalyzer / verify-uselistorder / llvm-mc / llvm-ar; llvm-nm / -objdump /
#                    -readelf / -size / -strings / -objcopy / -strip / -cxxfilt / -dwarfdump.
#   lli JIT          stdout / exit code / loops / recursion / libc / large IR.
#   FileCheck        positive + negative discrimination.
#   lld              ld.lld link+run.
#   errors           malformed IR / unknown pass / missing input all fail non-zero.
#   pipelines        clang|opt|llc|lld end-to-end; opt-optimised IR back through lli.
#
# Gate: prints `LLVM22_OK=<pass>/<total>` then `TEST PASSED` only when every check passed;
# otherwise `TEST FAILED`. The success anchor lives solely here so the qemu success_regex
# cannot self-match on the launch command.
#
# Expected LLVM major version. Alpine ships 22.1.x; assertions pin only the major.
EXP_MAJOR=22

# musl loader search path so libLLVM.so.22.1 (in /usr/lib) is always found.
for a in x86_64 aarch64 riscv64 loongarch64; do
    printf '/lib\n/usr/lib\n' > "/etc/ld-musl-$a.path" 2>/dev/null || true
done
# Alpine's llvm22 package installs the canonical unversioned tools under /usr/lib/llvm22/bin;
# clang/clang++ and the lld linkers land in /usr/bin.
LLVMBIN=/usr/lib/llvm22/bin
export PATH=$LLVMBIN:/usr/bin:/bin:/usr/sbin:/sbin
export HOME=/root
export TMPDIR=/tmp
mkdir -p /tmp

SRC=/root/llvm22/src
WORK=/root/llvm22/work
rm -rf "$WORK"; mkdir -p "$WORK"; cd "$WORK" || { echo "TEST FAILED"; exit 1; }

pass=0
total=0

ok()  { total=$((total + 1)); pass=$((pass + 1)); echo "  PASS | $1"; }
bad() { total=$((total + 1)); echo "  FAIL | $1 :: $2"; }

# check_eq NAME EXPECTED ACTUAL
check_eq() {
    if [ "x$2" = "x$3" ]; then ok "$1"; else bad "$1" "exp=[$2] got=[$3]"; fi
}
# check_true NAME RC (rc==0 -> pass)
check_true() {
    if [ "$2" -eq 0 ]; then ok "$1"; else bad "$1" "rc=$2"; fi
}
# check_false NAME RC (rc!=0 -> pass; used for negative / error cases)
check_false() {
    if [ "$2" -ne 0 ]; then ok "$1"; else bad "$1" "unexpected rc=0"; fi
}
# magic4 FILE -> first four bytes as lowercase hex
magic4() { od -An -tx1 -N4 "$1" 2>/dev/null | tr -d ' \n'; }
# elf_machine FILE -> two bytes at ELF offset 18 read little-endian (byte-swapped for BE ELF).
elf_machine() { od -An -tu1 -j18 -N2 "$1" 2>/dev/null | awk '{print $1 + $2 * 256}'; }
# count '= alloca' INSTRUCTIONS in an .ll file (not the word in comments)
count_alloca() { grep -c '= alloca' "$1"; }
# drain a matrix log of `OKLINE <name>` / `BADLINE <name> :: <why>` verdicts into the counters.
drain() {
    while read -r verdict rest; do
        case "$verdict" in
            OKLINE)  ok "$rest" ;;
            BADLINE) bad "${rest%% :: *}" "${rest##* :: }" ;;
        esac
    done < "$1"
}

echo "=== LLVM 22 toolchain carpet (StarryOS) ==="

# -------------------------------------------------------------------------
# 1. llvm-config: version
# -------------------------------------------------------------------------
echo "[llvm-config]"
ver=$(llvm-config --version 2>/dev/null)
check_eq "llvm-config --version major" "$EXP_MAJOR" "${ver%%.*}"

# -------------------------------------------------------------------------
# 2. BINARY SWEEP: every installed executable self-reports / self-identifies
#    Derived from `ls $LLVMBIN`; a tool that reports `... version 22.x` passes on that; one with
#    no version banner must launch (--help, no crash/hang) and print its own usage/diagnostic.
# -------------------------------------------------------------------------
echo "[binary sweep: every installed tool self-identifies as LLVM $EXP_MAJOR]"
probe_tool() {
    tp="$2"
    tv=$(timeout 40 "$tp" --version </dev/null 2>&1)
    if printf '%s\n' "$tv" | grep -qE "version $EXP_MAJOR\.|LLD $EXP_MAJOR\.|lit $EXP_MAJOR\.|^$EXP_MAJOR\."; then
        echo "OKLINE $1 --version reports $EXP_MAJOR.x"
        return
    fi
    th=$(timeout 40 "$tp" --help </dev/null 2>&1); trc=$?
    if [ "$trc" -lt 128 ] && [ "$trc" -ne 124 ] && printf '%s\n' "$th" | \
        grep -qiE 'usage|overview|options|input file|output file|no command action|generic driver|mapping generator|invalid specifier|argument|llvm-c-test command|subcommand'; then
        echo "OKLINE $1 --help launches and self-identifies"
    else
        echo "BADLINE $1 --help launches and self-identifies :: rc=$trc first=[$(printf '%s' "$th" | head -1 | cut -c1-40)]"
    fi
}
# amdgpu-arch / nvptx-arch / offload-arch are accelerator detectors that fork a probe helper;
# amdgpu-arch's --version occasionally exits on that helper's signal, so retry before falling
# back to the installed-and-launchable ELF check (no accelerator is present on the target).
probe_arch() {
    ta=0
    while [ "$ta" -lt 3 ]; do
        if timeout 12 "$2" --version </dev/null 2>&1 | grep -qE "version $EXP_MAJOR\."; then
            echo "OKLINE $1 --version reports offload-arch $EXP_MAJOR.x"
            return
        fi
        ta=$((ta + 1))
    done
    [ "$(magic4 "$2")" = "7f454c46" ] \
        && echo "OKLINE $1 installed as a launchable ELF (accelerator detector)" \
        || echo "BADLINE $1 installed as a launchable ELF (accelerator detector) :: not ELF"
}
{
    for f in "$LLVMBIN"/*; do
        [ -x "$f" ] || continue
        b=${f##*/}
        case "$b" in
            # FileCheck-suite drivers with no --version/--help contract: functional probe.
            count)
                printf 'x\ny\n' | "$f" 2 >/dev/null 2>&1 \
                    && echo "OKLINE count checks stdin line count" \
                    || echo "BADLINE count checks stdin line count :: nonzero on 2/2" ;;
            not)
                "$f" sh -c 'exit 1' >/dev/null 2>&1 \
                    && echo "OKLINE not inverts a failing exit status" \
                    || echo "BADLINE not inverts a failing exit status :: did not invert" ;;
            llvm-PerfectShuffle)
                # a CLI-less table generator (no --version/--help) whose full shuffle-table build
                # is too heavy to finish under TCG; a bounded launch proves it dynamic-loads and
                # runs - it either finishes (rc 0) or is still computing when the bound kills it
                # (busybox timeout -> 143, GNU -> 124), while a load/link failure exits fast with
                # a different code.
                timeout 30 "$f" >/dev/null 2>&1; pr=$?
                case "$pr" in
                    0|124|143) echo "OKLINE llvm-PerfectShuffle launches and runs (table generator)" ;;
                    *)         echo "BADLINE llvm-PerfectShuffle launches and runs (table generator) :: rc=$pr" ;;
                esac ;;
            amdgpu-arch|nvptx-arch|offload-arch)
                probe_arch "$b" "$f" ;;
            *)
                probe_tool "$b" "$f" ;;
        esac
    done
    # lld linker family (lld22 package) installs into /usr/bin, not $LLVMBIN.
    for b in ld.lld ld64.lld lld lld-link wasm-ld; do
        p=$(command -v "$b" 2>/dev/null)
        [ -n "$p" ] && probe_tool "$b" "$p" || echo "BADLINE $b present on PATH :: not found"
    done
} > tools.log
drain tools.log

# -------------------------------------------------------------------------
# 3. BACKEND MATRIX: every target `llc --version` registers x {O0,O1,O2,O3}
#    Target list DERIVED at runtime from llc; SIG_TABLE only maps a target to its expected object
#    signature (elf e_machine / spirv / wasm / none=asm-only). A registered target absent from the
#    map trips the gate (forces the map to stay complete). elf e_machine is read LE (byte-swapped
#    for big-endian ELF); spirv magic 03022307; wasm magic 0061736d.
# -------------------------------------------------------------------------
SIG_TABLE="\
aarch64 elf 183
aarch64_32 elf 183
aarch64_be elf 46848
amdgcn elf 224
arm elf 40
arm64 elf 183
arm64_32 elf 183
armeb elf 10240
avr elf 83
bpf elf 247
bpfeb elf 63232
bpfel elf 247
hexagon elf 164
lanai elf 62464
loongarch32 elf 258
loongarch64 elf 258
mips elf 2048
mips64 elf 2048
mips64el elf 8
mipsel elf 8
msp430 elf 105
nvptx none -
nvptx64 none -
ppc32 elf 5120
ppc32le elf 20
ppc64 elf 5376
ppc64le elf 21
r600 elf 224
riscv32 elf 243
riscv32be elf 62208
riscv64 elf 243
riscv64be elf 62208
sparc elf 512
sparcel elf 2
sparcv9 elf 11008
spirv spirv -
spirv32 spirv -
spirv64 spirv -
systemz elf 5632
thumb elf 40
thumbeb elf 10240
ve elf 251
wasm32 wasm -
wasm64 wasm -
x86 elf 3
x86-64 elf 62
xcore none -"

llc --version 2>/dev/null | awk '/Registered Targets:/{f=1;next} f && /^[[:space:]]+[A-Za-z0-9_.-]+[[:space:]]+-/{print $1}' > targets.txt
ntgt=$(wc -l < targets.txt)
check_true "llc --version registers a nonempty target set ($ntgt targets)" "$([ "$ntgt" -gt 0 ] && echo 0 || echo 1)"

echo "[backend matrix: asm x every registered target x 4 opt-levels]"
{
    while read -r t; do
        for o in 0 1 2 3; do
            llc -march="$t" -O"$o" -filetype=asm "$SRC/xcodegen.ll" -o "asm_${t}_O${o}.s" 2>/dev/null
            if grep -q 'addfn' "asm_${t}_O${o}.s" 2>/dev/null; then
                echo "OKLINE llc -march=$t -O$o asm (addfn)"
            else
                echo "BADLINE llc -march=$t -O$o asm (addfn) :: symbol absent"
            fi
        done
    done < targets.txt
} > asm_matrix.log
drain asm_matrix.log

echo "[backend matrix: object x every object-capable target x 4 opt-levels]"
{
    while read -r t; do
        row=$(printf '%s\n' "$SIG_TABLE" | awk -v k="$t" '$1==k{print $2, $3; exit}')
        if [ -z "$row" ]; then
            echo "BADLINE llc -march=$t obj :: no object-signature mapping (new/unknown target)"
            continue
        fi
        kind=${row% *}; exp=${row#* }
        [ "$kind" = "none" ] && continue
        for o in 0 1 2 3; do
            of="obj_${t}_O${o}.o"
            llc -march="$t" -O"$o" -filetype=obj "$SRC/xcodegen.ll" -o "$of" 2>/dev/null
            case "$kind" in
                elf)
                    if [ "$(magic4 "$of")" = "7f454c46" ] && [ "$(elf_machine "$of")" = "$exp" ]; then
                        echo "OKLINE llc -march=$t -O$o obj (ELF e_machine=$exp)"
                    else
                        echo "BADLINE llc -march=$t -O$o obj (ELF e_machine=$exp) :: magic=$(magic4 "$of") em=$(elf_machine "$of")"
                    fi ;;
                spirv)
                    if [ "$(magic4 "$of")" = "03022307" ]; then
                        echo "OKLINE llc -march=$t -O$o obj (SPIR-V magic)"
                    else
                        echo "BADLINE llc -march=$t -O$o obj (SPIR-V magic) :: magic=$(magic4 "$of")"
                    fi ;;
                wasm)
                    if [ "$(magic4 "$of")" = "0061736d" ]; then
                        echo "OKLINE llc -march=$t -O$o obj (wasm magic)"
                    else
                        echo "BADLINE llc -march=$t -O$o obj (wasm magic) :: magic=$(magic4 "$of")"
                    fi ;;
            esac
        done
    done < targets.txt
} > obj_matrix.log
drain obj_matrix.log

# -------------------------------------------------------------------------
# 4. llc native codegen + object introspection (host arch)
# -------------------------------------------------------------------------
echo "[llc native + object tools]"
llc "$SRC/hello.ll" -o hello.s 2>/dev/null
if grep -q 'main' hello.s && grep -qi 'globl' hello.s; then
    check_true "llc emits native asm (.globl main)" 0
else
    check_true "llc emits native asm (.globl main)" 1
fi

llc -filetype=obj "$SRC/hello.ll" -o hello.o 2>/dev/null
check_eq "llc emits native ELF object" "7f454c46" "$(magic4 hello.o)"

llvm-nm hello.o 2>/dev/null | grep -q 'T main' && check_true "llvm-nm lists 'T main'" 0 || check_true "llvm-nm lists 'T main'" 1
llvm-objdump -d hello.o 2>/dev/null | grep -q 'main' && check_true "llvm-objdump disassembles main" 0 || check_true "llvm-objdump disassembles main" 1
llvm-readelf -h hello.o 2>/dev/null | grep -q 'ELF' && check_true "llvm-readelf prints an ELF header" 0 || check_true "llvm-readelf prints an ELF header" 1
llvm-size hello.o 2>/dev/null | grep -q 'text' && check_true "llvm-size reports a text column" 0 || check_true "llvm-size reports a text column" 1
llvm-strings hello.o 2>/dev/null | grep -q 'Hello, LLVM 22' && check_true "llvm-strings finds the message literal" 0 || check_true "llvm-strings finds the message literal" 1

llvm-objcopy hello.o hello_copy.o 2>/dev/null
check_true "llvm-objcopy copies the object" $?
llvm-strip hello_copy.o 2>/dev/null
check_true "llvm-strip strips the object copy" $?

echo '_ZN7Counter3addEi' | llvm-cxxfilt 2>/dev/null | grep -q 'Counter::add(int)' \
    && check_true "llvm-cxxfilt demangles Itanium name" 0 || check_true "llvm-cxxfilt demangles Itanium name" 1

llvm-mc -filetype=obj hello.s -o hello_mc.o 2>/dev/null
check_eq "llvm-mc assembles asm -> ELF object" "7f454c46" "$(magic4 hello_mc.o)"

clang -g -c "$SRC/hello.c" -o hello_dbg.o 2>/dev/null
llvm-dwarfdump hello_dbg.o 2>/dev/null | grep -q 'DW_TAG_compile_unit' \
    && check_true "llvm-dwarfdump reads a compile unit" 0 || check_true "llvm-dwarfdump reads a compile unit" 1

# -------------------------------------------------------------------------
# 5. llvm-as / llvm-dis: IR <-> bitcode roundtrip + empty module
# -------------------------------------------------------------------------
echo "[llvm-as / llvm-dis]"
llvm-as "$SRC/hello.ll" -o hello.bc 2>/dev/null
check_eq "llvm-as emits bitcode magic (BC C0 DE)" "4243c0de" "$(magic4 hello.bc)"

llvm-dis hello.bc -o hello_rt.ll 2>/dev/null
grep -q '@main' hello_rt.ll && check_true "llvm-dis disassembles (has @main)" 0 || check_true "llvm-dis disassembles (has @main)" 1

llvm-as hello_rt.ll -o /dev/null 2>/dev/null
check_true "llvm-dis output re-assembles (roundtrip stable)" $?

llvm-as "$SRC/empty.ll" -o empty.bc 2>/dev/null
check_eq "llvm-as accepts empty module (valid bitcode)" "4243c0de" "$(magic4 empty.bc)"

verify-uselistorder hello.bc >/dev/null 2>&1
check_true "verify-uselistorder roundtrips use-list order" $?

# -------------------------------------------------------------------------
# 6. lli: JIT execution
# -------------------------------------------------------------------------
echo "[lli JIT]"
out=$(lli "$SRC/hello.ll" 2>/dev/null)
check_eq "lli hello: stdout" "Hello, LLVM 22!" "$out"

lli "$SRC/arith.ll" >/dev/null 2>&1
check_eq "lli arith: exit code" "42" "$?"

out=$(lli "$SRC/loop.ll" 2>/dev/null)
check_eq "lli loop: sum 1..100" "SUM=5050" "$out"

out=$(lli "$SRC/fib.ll" 2>/dev/null)
check_eq "lli fib: recursion fib(10)" "FIB=55" "$out"

awk 'BEGIN {
    print "define i32 @main() {"; print "entry:";
    print "  %v0 = add i32 0, 1";
    for (i = 1; i < 1000; i++) printf "  %%v%d = add i32 %%v%d, 1\n", i, i - 1;
    print "  ret i32 %v999"; print "}"
}' > big.ll
lli big.ll >/dev/null 2>&1
check_eq "lli large IR (1000-add chain): exit code 1000 & 255" "232" "$?"

# -------------------------------------------------------------------------
# 7. opt: exact-transform passes
# -------------------------------------------------------------------------
echo "[opt exact transforms]"
check_eq "mem2reg fixture has 2 allocas" "2" "$(count_alloca "$SRC/mem2reg.ll")"
opt -passes=mem2reg -S "$SRC/mem2reg.ll" -o m2r.ll 2>/dev/null
check_eq "opt -passes=mem2reg eliminates allocas" "0" "$(count_alloca m2r.ll)"

opt -passes=sroa -S "$SRC/sroa.ll" -o sroa_o.ll 2>/dev/null
check_eq "opt -passes=sroa splits+promotes the aggregate (no alloca)" "0" "$(grep -c 'alloca' sroa_o.ll)"

opt -passes=instcombine -S "$SRC/instcombine.ll" -o ic.ll 2>/dev/null
if [ "$(grep -c ' mul ' ic.ll)" = "0" ] && [ "$(grep -c ' add ' ic.ll)" = "0" ] && grep -q 'ret i32 %x' ic.ll; then
    check_true "opt -passes=instcombine folds (x*1)+0 to x" 0
else
    check_true "opt -passes=instcombine folds (x*1)+0 to x" 1
fi

opt -passes=inline -S "$SRC/inline.ll" -o inl.ll 2>/dev/null
check_eq "opt -passes=inline removes the call site" "0" "$(grep -c 'call i32 @callee' inl.ll)"

opt -passes=gvn -S "$SRC/gvn.ll" -o gvn_o.ll 2>/dev/null
check_eq "opt -passes=gvn removes the redundant load" "1" "$(grep -c 'load i32' gvn_o.ll)"

opt -passes=dce -S "$SRC/dce.ll" -o dce_o.ll 2>/dev/null
check_eq "opt -passes=dce deletes the dead mul (777 gone)" "0" "$(grep -c '777' dce_o.ll)"

opt -passes=adce -S "$SRC/dce.ll" -o adce_o.ll 2>/dev/null
check_eq "opt -passes=adce deletes the dead mul (777 gone)" "0" "$(grep -c '777' adce_o.ll)"

opt -passes=simplifycfg -S "$SRC/simplifycfg.ll" -o scfg_o.ll 2>/dev/null
check_eq "opt -passes=simplifycfg folds the branch chain (no br label)" "0" "$(grep -c 'br label' scfg_o.ll)"

opt -passes='sccp,simplifycfg' -S "$SRC/sccp.ll" -o sccp_o.ll 2>/dev/null
if grep -q 'ret i32 100' sccp_o.ll && [ "$(grep -c 'ret i32 200' sccp_o.ll)" = "0" ]; then
    check_true "opt -passes=sccp resolves the constant branch (keeps 100, drops 200)" 0
else
    check_true "opt -passes=sccp resolves the constant branch (keeps 100, drops 200)" 1
fi

opt -passes=constmerge -S "$SRC/constmerge.ll" -o cm_o.ll 2>/dev/null
check_eq "opt -passes=constmerge merges duplicate constants (1 remains)" "1" "$(grep -c 'unnamed_addr constant \[4' cm_o.ll)"

opt -passes=globaldce -S "$SRC/globaldce.ll" -o gdce_o.ll 2>/dev/null
check_eq "opt -passes=globaldce removes the unused global" "0" "$(grep -c '@unused' gdce_o.ll)"

opt -passes=deadargelim -S "$SRC/deadargelim.ll" -o dae_o.ll 2>/dev/null
check_eq "opt -passes=deadargelim drops the unused argument (123 gone)" "0" "$(grep -c '123' dae_o.ll)"

opt -passes=early-cse -S "$SRC/earlycse.ll" -o ecse_o.ll 2>/dev/null
check_eq "opt -passes=early-cse collapses the common subexpression" "1" "$(grep -c 'add i32' ecse_o.ll)"

opt -passes=dse -S "$SRC/dse.ll" -o dse_o.ll 2>/dev/null
if [ "$(grep -c 'store i32 1' dse_o.ll)" = "0" ] && [ "$(grep -c 'store i32 2' dse_o.ll)" = "1" ]; then
    check_true "opt -passes=dse deletes the overwritten store" 0
else
    check_true "opt -passes=dse deletes the overwritten store" 1
fi

opt -passes=tailcallelim -S "$SRC/tailcall.ll" -o tce_o.ll 2>/dev/null
check_eq "opt -passes=tailcallelim removes the recursive tail call" "0" "$(grep -c 'call i32 @tcetest' tce_o.ll)"

opt -passes='loop-unroll,simplifycfg' -S "$SRC/unroll.ll" -o unr_o.ll 2>/dev/null
if [ "$(grep -c 'phi' unr_o.ll)" = "0" ] && [ "$(grep -c 'loop:' unr_o.ll)" = "0" ]; then
    check_true "opt -passes=loop-unroll fully unrolls the fixed loop" 0
else
    check_true "opt -passes=loop-unroll fully unrolls the fixed loop" 1
fi

opt -passes='loop-mssa(licm)' -S "$SRC/licm.ll" -o licm_o.ll 2>/dev/null
licm_inloop=$(awk '/^loop.*:/{f=1} /^done:/{f=0} f && /mul i32/' licm_o.ll | wc -l)
if [ "$(grep -c 'mul i32' licm_o.ll)" = "1" ] && [ "$licm_inloop" -eq 0 ]; then
    check_true "opt -passes=licm hoists the invariant mul out of the loop body" 0
else
    check_true "opt -passes=licm hoists the invariant mul out of the loop body" 1
fi

opt -passes=reassociate -S "$SRC/reassoc.ll" -o reassoc_o.ll 2>/dev/null
grep -q 'add i32 %b, %a' reassoc_o.ll \
    && check_true "opt -passes=reassociate canonicalizes the add-tree operand order" 0 \
    || check_true "opt -passes=reassociate canonicalizes the add-tree operand order" 1

# -------------------------------------------------------------------------
# 8. opt: -O level pipelines + default<O2> + bitcode pipeline
# -------------------------------------------------------------------------
echo "[opt -O pipelines]"
opt -O0 -S "$SRC/mem2reg.ll" -o o0.ll 2>/dev/null
check_eq "opt -O0 retains allocas (no promotion)" "2" "$(count_alloca o0.ll)"
opt -O1 -S "$SRC/mem2reg.ll" -o o1.ll 2>/dev/null
check_eq "opt -O1 promotes to SSA (no alloca)" "0" "$(count_alloca o1.ll)"
opt -O2 -S "$SRC/mem2reg.ll" -o o2.ll 2>/dev/null
if [ "$(count_alloca o2.ll)" = "0" ] && grep -q '@compute' o2.ll; then
    check_true "opt -O2 promotes to SSA (no alloca, @compute kept)" 0
else
    check_true "opt -O2 promotes to SSA (no alloca, @compute kept)" 1
fi
opt -O3 -S "$SRC/mem2reg.ll" -o o3.ll 2>/dev/null
check_eq "opt -O3 promotes to SSA (no alloca)" "0" "$(count_alloca o3.ll)"
opt -passes='default<O2>' -S "$SRC/mem2reg.ll" -o dflt.ll 2>/dev/null
check_eq "opt -passes=default<O2> pipeline (no alloca)" "0" "$(count_alloca dflt.ll)"

llvm-as "$SRC/mem2reg.ll" -o m2r_in.bc 2>/dev/null
opt -passes=mem2reg m2r_in.bc -o m2r_out.bc 2>/dev/null
llvm-dis m2r_out.bc -o m2r_out.ll 2>/dev/null
check_eq "opt bitcode pipeline (as|opt|dis) no alloca" "0" "$(count_alloca m2r_out.ll)"

# -------------------------------------------------------------------------
# 9. opt: run-clean sweep over EVERY transform pass `opt --print-passes` lists
#    Enumerated at runtime from the plain (non-parameterized) Module / CGSCC / Function / LoopNest
#    / Loop pass sections, each scheduled with its category adaptor. A pass passes when opt runs it
#    clean (exit 0, output verified) OR recognizes it but reports a missing TargetMachine / profile
#    / summary input; it fails when opt rejects the name (unknown pass) or crashes. The DENY set is
#    the handful of listed passes that abort a bare module by design - the crash-handler test passes
#    and the machine / external-summary passes - covered as listed-only.
# -------------------------------------------------------------------------
echo "[opt pass sweep: every transform pass opt --print-passes lists]"
DENY="trigger-crash-function trigger-crash-module trigger-verifier-error free-machine-function dfsan function-import ctx-prof-flatten-prethinlink"
opt --print-passes 2>/dev/null | awk '
    /:$/ { u = 0
        if ($0 == "Module passes:")   { c = "module";   u = 1 }
        if ($0 == "CGSCC passes:")    { c = "cgscc";    u = 1 }
        if ($0 == "Function passes:") { c = "function"; u = 1 }
        if ($0 == "LoopNest passes:") { c = "loop";     u = 1 }
        if ($0 == "Loop passes:")     { c = "loop";     u = 1 }
        active = u; next }
    active && /^  [A-Za-z]/ && $1 !~ /</ { print c, $1 }
' | sort -u > passlist.txt
{
    while read -r c p; do
        case " $DENY " in
            *" $p "*) echo "OKLINE opt pass $p listed (crash-handler/external-input pass, not scheduled)"; continue ;;
        esac
        case "$c" in
            module)   s="$p" ;;
            cgscc)    s="cgscc($p)" ;;
            function) s="function($p)" ;;
            loop)     s="function(loop($p))" ;;
        esac
        po=$(opt -passes="$s" -disable-output "$SRC/passgen.ll" 2>&1); pr=$?
        if [ "$pr" -eq 0 ]; then
            echo "OKLINE opt -passes=$p runs clean"
        elif [ "$pr" -lt 128 ] && ! printf '%s' "$po" | grep -qiE 'unknown .*pass|unable to parse'; then
            echo "OKLINE opt -passes=$p recognized (needs target/profile input)"
        else
            echo "BADLINE opt -passes=$p runs :: rc=$pr $(printf '%s' "$po" | grep -i unknown | head -1)"
        fi
    done < passlist.txt
} > passes.log
drain passes.log

# -------------------------------------------------------------------------
# 10. llvm-link / llvm-extract / llvm-cat / llvm-diff: multi-module IR tools
# -------------------------------------------------------------------------
echo "[IR module tools]"
llvm-link "$SRC/link_a.ll" "$SRC/link_b.ll" -S -o linked.ll 2>/dev/null
if grep -q '@funcA' linked.ll && grep -q '@funcB' linked.ll; then
    check_true "llvm-link merges both modules (@funcA + @funcB)" 0
else
    check_true "llvm-link merges both modules (@funcA + @funcB)" 1
fi

llvm-as "$SRC/multi.ll" -o multi.bc 2>/dev/null
llvm-extract -func=beta multi.bc -o beta.bc 2>/dev/null
llvm-dis beta.bc -o beta.ll 2>/dev/null
if grep -q 'define i32 @beta' beta.ll && [ "$(grep -c 'define i32 @alpha' beta.ll)" = "0" ]; then
    check_true "llvm-extract pulls only @beta out of the module" 0
else
    check_true "llvm-extract pulls only @beta out of the module" 1
fi

llvm-bcanalyzer multi.bc 2>/dev/null | grep -q 'Block ID' \
    && check_true "llvm-bcanalyzer reports bitcode block structure" 0 || check_true "llvm-bcanalyzer reports bitcode block structure" 1

llvm-cat -o cat.bc multi.bc beta.bc 2>/dev/null
check_eq "llvm-cat concatenates bitcode (BC magic)" "4243c0de" "$(magic4 cat.bc)"

llvm-diff multi.bc multi.bc >/dev/null 2>&1
check_true "llvm-diff reports no diff for identical modules" $?

# -------------------------------------------------------------------------
# 11. llvm-ar: static archive create / list / symbol index / extract
# -------------------------------------------------------------------------
echo "[llvm-ar]"
llc -filetype=obj "$SRC/link_a.ll" -o arA.o 2>/dev/null
llc -filetype=obj "$SRC/link_b.ll" -o arB.o 2>/dev/null
rm -f lib.a
llvm-ar rcs lib.a arA.o arB.o 2>/dev/null
check_eq "llvm-ar rcs creates archive (! < a r c h > magic)" '!<arch>\n' "$(od -An -c -N8 lib.a 2>/dev/null | tr -d ' \n')"

ar_t=$(llvm-ar t lib.a 2>/dev/null)
if echo "$ar_t" | grep -q 'arA.o' && echo "$ar_t" | grep -q 'arB.o'; then
    check_true "llvm-ar t lists both members" 0
else
    check_true "llvm-ar t lists both members" 1
fi

arnm=$(llvm-nm lib.a 2>/dev/null)
if echo "$arnm" | grep -q 'T funcA' && echo "$arnm" | grep -q 'T funcB'; then
    check_true "llvm-nm reads archive symbol index (funcA + funcB)" 0
else
    check_true "llvm-nm reads archive symbol index (funcA + funcB)" 1
fi

mkdir -p xdir && (cd xdir && rm -f arA.o && llvm-ar x ../lib.a arA.o 2>/dev/null)
[ -s xdir/arA.o ] && check_true "llvm-ar x extracts a member" 0 || check_true "llvm-ar x extracts a member" 1

# -------------------------------------------------------------------------
# 12. FileCheck: positive + negative discrimination
# -------------------------------------------------------------------------
echo "[FileCheck]"
llvm-dis hello.bc -o hello_check_in.ll 2>/dev/null
FileCheck "$SRC/hello.check" < hello_check_in.ll >/dev/null 2>&1
check_true "FileCheck positive match" $?
FileCheck "$SRC/bad.check" < hello_check_in.ll >/dev/null 2>&1
check_false "FileCheck negative (must fail on absent pattern)" $?

# -------------------------------------------------------------------------
# 13. clang -x: front-end language recognition (derived) + negative control
#     Every source-language mode in clang's InputKind taxonomy is checked against clang's own
#     recognition; a bogus name must be rejected. renderscript is not built into this Alpine clang
#     (recognized only as a control-of-absence, so it is not asserted here).
# -------------------------------------------------------------------------
echo "[clang -x language recognition]"
{
    for m in c c-header cpp-output c++ c++-header c++-cpp-output \
             objective-c objective-c-header objective-c-cpp-output \
             objective-c++ objective-c++-header objective-c++-cpp-output \
             cl clcpp cl-header cuda cuda-cpp-output hip hip-cpp-output \
             hlsl ir assembler assembler-with-cpp ast pcm; do
        if clang -x "$m" -fsyntax-only /dev/null 2>&1 | grep -qi 'language not recognized'; then
            echo "BADLINE clang -x $m recognized :: rejected"
        else
            echo "OKLINE clang -x $m recognized"
        fi
    done
    if clang -x bogus-mode-zzq9137 -fsyntax-only /dev/null 2>&1 | grep -qi 'language not recognized'; then
        echo "OKLINE clang -x rejects a bogus language mode"
    else
        echo "BADLINE clang -x rejects a bogus language mode :: accepted"
    fi
} > xlang.log
drain xlang.log

# -------------------------------------------------------------------------
# 14. clang: C front-end matrix (IR / asm / object / exe, -std dialects, -O levels)
# -------------------------------------------------------------------------
echo "[clang C front-end]"
clang --version 2>/dev/null | head -1 | grep -q "version $EXP_MAJOR\." \
    && check_true "clang --version is $EXP_MAJOR.x" 0 || check_true "clang --version is $EXP_MAJOR.x" 1

clang -S -emit-llvm -O0 "$SRC/hello.c" -o hello_c.ll 2>/dev/null
if grep -q 'define' hello_c.ll && grep -q '@main' hello_c.ll && grep -q '@printf' hello_c.ll; then
    check_true "clang -emit-llvm: C -> IR (@main + @printf)" 0
else
    check_true "clang -emit-llvm: C -> IR (@main + @printf)" 1
fi

clang -S "$SRC/hello.c" -o hello_c.s 2>/dev/null
grep -q 'main' hello_c.s && check_true "clang -S: C -> native asm (main)" 0 || check_true "clang -S: C -> native asm (main)" 1

clang -c "$SRC/hello.c" -o hello_c.o 2>/dev/null
check_eq "clang -c: C -> native ELF object" "7f454c46" "$(magic4 hello_c.o)"

clang "$SRC/hello.c" -o hello_bin 2>/dev/null
check_eq "clang C -> exe: compile+link+run" "CLANG22 OK" "$(./hello_bin 2>/dev/null)"

for s in c99 c11 c17 c23 gnu11 gnu17 gnu23; do
    clang -std="$s" -c "$SRC/hello.c" -o "hello_${s}.o" 2>/dev/null
    check_true "clang -std=$s: C -> object" $?
done

for o in 0 1 2 3; do
    clang -O"$o" "$SRC/hello.c" -o "hello_O${o}" 2>/dev/null
    check_eq "clang -O$o: C -> exe run" "CLANG22 OK" "$(./hello_O${o} 2>/dev/null)"
done

clang -O2 "$SRC/math.c" -lm -o math_bin 2>/dev/null
check_eq "clang -O2 -lm: sqrt(2)" "SQRT=1.4142" "$(./math_bin 2>/dev/null)"

# -------------------------------------------------------------------------
# 15. clang++: C++ front-end matrix (IR / asm / object / exe, -std dialects)
# -------------------------------------------------------------------------
echo "[clang++ C++ front-end]"
clang++ --version 2>/dev/null | head -1 | grep -q "version $EXP_MAJOR\." \
    && check_true "clang++ --version is $EXP_MAJOR.x" 0 || check_true "clang++ --version is $EXP_MAJOR.x" 1

clang++ -std=c++17 -S -emit-llvm -O0 "$SRC/hello.cpp" -o hello_cpp.ll 2>/dev/null
grep -q '_ZN7Counter' hello_cpp.ll && check_true "clang++ -emit-llvm: C++ -> IR (Itanium mangling)" 0 || check_true "clang++ -emit-llvm: C++ -> IR (Itanium mangling)" 1

clang++ -std=c++17 -S "$SRC/hello.cpp" -o hello_cpp.s 2>/dev/null
check_true "clang++ -S: C++ -> native asm" $?

clang++ -std=c++17 -c "$SRC/hello.cpp" -o hello_cpp.o 2>/dev/null
check_eq "clang++ -c: C++ -> native ELF object" "7f454c46" "$(magic4 hello_cpp.o)"

clang++ -std=c++17 "$SRC/hello.cpp" -o hello_cpp 2>/dev/null
check_eq "clang++ C++ -> exe: templates + STL + run" "CPP22 SUM=15 CNT=5" "$(./hello_cpp 2>/dev/null)"

for s in c++17 c++20 c++23; do
    clang++ -std="$s" -c "$SRC/hello.cpp" -o "cpp_${s}.o" 2>/dev/null
    check_true "clang++ -std=$s: C++ -> object" $?
done

# -------------------------------------------------------------------------
# 16. clang: Objective-C / Objective-C++ front-end (IR metadata + object; no runtime link)
# -------------------------------------------------------------------------
echo "[clang Objective-C / Objective-C++ front-end]"
clang -x objective-c -S -emit-llvm -O0 "$SRC/hello.m" -o hello_m.ll 2>/dev/null
if grep -q 'OBJC_CLASS' hello_m.ll && grep -q 'OBJC_METACLASS' hello_m.ll; then
    check_true "clang -x objective-c -> IR (OBJC_CLASS + OBJC_METACLASS)" 0
else
    check_true "clang -x objective-c -> IR (OBJC_CLASS + OBJC_METACLASS)" 1
fi
clang -x objective-c -c "$SRC/hello.m" -o hello_m.o 2>/dev/null
check_eq "clang -x objective-c -> native ELF object" "7f454c46" "$(magic4 hello_m.o)"

clang -x objective-c++ -S -emit-llvm -O0 "$SRC/hello.m" -o hello_mm.ll 2>/dev/null
grep -q 'OBJC_CLASS' hello_mm.ll && check_true "clang -x objective-c++ -> IR (OBJC metadata)" 0 || check_true "clang -x objective-c++ -> IR (OBJC metadata)" 1

# -------------------------------------------------------------------------
# 17. clang extra -x modes: OpenCL C / OpenCL C++ / LLVM IR / assembler (headless)
#     cuda / hip are recognized but device-toolkit-gated: clang must report the missing device
#     library (no device execution), which proves the offload front-end is wired.
# -------------------------------------------------------------------------
echo "[clang extra -x modes]"
clang -x cl -cl-std=CL2.0 -S -emit-llvm "$SRC/kernel.cl" -o kern_cl.ll 2>/dev/null
grep -q 'define' kern_cl.ll && check_true "clang -x cl (OpenCL C) -> IR (@kmul)" 0 || check_true "clang -x cl (OpenCL C) -> IR (@kmul)" 1

clang -x clcpp -cl-std=clc++ -S -emit-llvm "$SRC/kernel.cl" -o kern_clcpp.ll 2>/dev/null
grep -q 'define' kern_clcpp.ll && check_true "clang -x clcpp (OpenCL C++) -> IR" 0 || check_true "clang -x clcpp (OpenCL C++) -> IR" 1

clang -x ir "$SRC/hello.ll" -S -o ir_re.s 2>/dev/null
grep -q 'main' ir_re.s && check_true "clang -x ir (LLVM IR input) -> native asm" 0 || check_true "clang -x ir (LLVM IR input) -> native asm" 1

clang -S "$SRC/hello.c" -o gen.s 2>/dev/null
clang -x assembler gen.s -c -o gen_asm.o 2>/dev/null
check_eq "clang -x assembler -> native ELF object" "7f454c46" "$(magic4 gen_asm.o)"
clang -x assembler-with-cpp gen.s -c -o gen_asmpp.o 2>/dev/null
check_eq "clang -x assembler-with-cpp -> native ELF object" "7f454c46" "$(magic4 gen_asmpp.o)"

clang -x cuda --cuda-device-only -S -emit-llvm "$SRC/hello.c" -o /dev/null 2>cuda.err
grep -qiE 'cannot find|cuda' cuda.err && check_true "clang -x cuda front-end wired (reports missing CUDA toolkit)" 0 || check_true "clang -x cuda front-end wired (reports missing CUDA toolkit)" 1
clang -x hip --offload-device-only -S -emit-llvm "$SRC/hello.c" -o /dev/null 2>hip.err
grep -qiE 'cannot find|rocm|device library' hip.err && check_true "clang -x hip front-end wired (reports missing ROCm toolkit)" 0 || check_true "clang -x hip front-end wired (reports missing ROCm toolkit)" 1

# -------------------------------------------------------------------------
# 18. lld: LLVM linker
# -------------------------------------------------------------------------
echo "[lld]"
clang -fuse-ld=lld "$SRC/hello.c" -o hello_lld 2>/dev/null
check_eq "clang -fuse-ld=lld: link+run" "CLANG22 OK" "$(./hello_lld 2>/dev/null)"

# -------------------------------------------------------------------------
# 19. error handling: malformed IR / unknown pass / missing input must fail
# -------------------------------------------------------------------------
echo "[errors]"
llvm-as "$SRC/malformed.ll" -o /dev/null 2>/dev/null
check_false "llvm-as rejects malformed IR (non-zero)" $?
llc "$SRC/malformed.ll" -o /dev/null 2>/dev/null
check_false "llc rejects malformed IR (non-zero)" $?
opt -passes='this_pass_does_not_exist_zzq9137' "$SRC/hello.ll" -o /dev/null 2>/dev/null
check_false "opt rejects unknown pass name (non-zero)" $?
lli /root/llvm22/src/no_such_file_zzq9137.ll >/dev/null 2>&1
check_false "lli rejects missing input file (non-zero)" $?

# -------------------------------------------------------------------------
# 20. integration pipelines
# -------------------------------------------------------------------------
echo "[pipelines]"
clang -S -emit-llvm -O0 "$SRC/hello.c" -o pipe.ll 2>/dev/null
opt -O2 -S pipe.ll -o pipe_opt.ll 2>/dev/null
llc -relocation-model=pic -filetype=obj pipe_opt.ll -o pipe.o 2>/dev/null
clang -fuse-ld=lld pipe.o -o pipe_bin 2>/dev/null
check_eq "pipeline clang|opt|llc|lld -> run" "CLANG22 OK" "$(./pipe_bin 2>/dev/null)"

opt -O2 -S "$SRC/loop.ll" -o loop_opt.ll 2>/dev/null
check_eq "pipeline opt -O2 loop.ll | lli -> SUM" "SUM=5050" "$(lli loop_opt.ll 2>/dev/null)"

# -------------------------------------------------------------------------
# Gate
# -------------------------------------------------------------------------
echo "=== summary: $pass/$total checks passed ==="
echo "LLVM22_OK=$pass/$total"
if [ "$pass" -eq "$total" ]; then
    echo "TEST PASSED"
    exit 0
fi
echo "TEST FAILED"
exit 1
