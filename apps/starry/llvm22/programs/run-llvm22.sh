#!/bin/sh
# run-llvm22.sh - on-target LLVM 22 toolchain carpet for StarryOS.
#
# Exercises the real musl-native LLVM 22 tools (installed from Alpine `apk add
# llvm22 llvm22-dev llvm22-test-utils clang22 lld22 g++ ...`) against a set of
# LLVM IR / C / C++ fixtures, one exact assertion per tool feature, spanning the
# seven coverage dimensions (documentation walk, --help/--version, per-option unit
# behaviour, functional output, boundary inputs, error handling, and pipelines):
#   llvm-config      version + built targets (cross-codegen capability)
#   --version sweep  every tool self-reports LLVM major 22
#   llvm-as/-dis     IR <-> bitcode roundtrip (bitcode magic, re-parse), empty module
#   lli              JIT: stdout + exit code, loops, recursion, libc calls, large IR
#   llc              IR -> asm / object; cross-codegen for all four built targets
#   llvm-nm/objdump  symbol table + disassembly of the emitted object
#   opt              mem2reg, -O0/-O1/-O2/-O3, default<O2>, instcombine, inline, bitcode
#   llvm-link        module merge
#   llvm-ar          static archive create / list / extract / symbol index
#   FileCheck        positive + negative (discrimination) match, --help
#   clang            C -> IR, C -> exe, -O2 -lm, -std= dialects
#   clang++          C++ -> IR (name mangling), C++ -> exe (templates + STL)
#   lld              ld.lld version + clang -fuse-ld=lld link+run
#   errors           malformed IR / unknown pass / missing input all fail non-zero
#   pipelines        clang|opt|llc|lld end-to-end, opt-optimised IR back through lli
#
# Gate: prints `LLVM22_OK=<pass>/<total>` then `TEST PASSED` only when every
# check passed; otherwise `TEST FAILED`. The success anchor lives solely here so
# the qemu success_regex cannot self-match on the launch command.
#
# Expected LLVM major version. Alpine ships 22.1.x; assertions pin only the major.
EXP_MAJOR=22

# musl loader search path so libLLVM.so.22.1 (in /usr/lib) is always found.
for a in x86_64 aarch64 riscv64 loongarch64; do
    printf '/lib\n/usr/lib\n' > "/etc/ld-musl-$a.path" 2>/dev/null || true
done
# Alpine's llvm22 package installs the unversioned tools (lli/llc/opt/llvm-config/
# FileCheck/llvm-*) under /usr/lib/llvm22/bin; only clang/clang++/clang-22/ld.lld land in /usr/bin.
export PATH=/usr/lib/llvm22/bin:/usr/bin:/bin:/usr/sbin:/sbin
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
# check_grep NAME PATTERN FILE  (fixed-string grep must match)
check_grep() {
    if grep -q "$2" "$3" 2>/dev/null; then ok "$1"; else bad "$1" "pattern [$2] absent"; fi
}
# check_version TOOL  (<tool> --version must report `LLVM version 22.x`)
check_version() {
    v=$("$1" --version 2>/dev/null)
    if echo "$v" | grep -qE "LLVM version $EXP_MAJOR\."; then
        ok "$1 --version is $EXP_MAJOR.x"
    else
        bad "$1 --version is $EXP_MAJOR.x" "got=[$(echo "$v" | grep -i version | head -1)]"
    fi
}
# ELF e_machine (2 bytes LE at offset 18)
elf_machine() {
    od -An -tu1 -j18 -N2 "$1" 2>/dev/null | awk '{print $1 + $2 * 256}'
}
# count '= alloca' INSTRUCTIONS in an .ll file (not the word in comments)
count_alloca() { grep -c '= alloca' "$1"; }

echo "=== LLVM 22 toolchain carpet (StarryOS) ==="

# -------------------------------------------------------------------------
# 1. llvm-config: version + cross-codegen target set
# -------------------------------------------------------------------------
echo "[llvm-config]"
ver=$(llvm-config --version 2>/dev/null)
maj=${ver%%.*}
check_eq "llvm-config --version major" "$EXP_MAJOR" "$maj"

tgts=$(llvm-config --targets-built 2>/dev/null)
echo "$tgts" | grep -qw X86        && check_true "targets-built has X86" 0        || check_true "targets-built has X86" 1
echo "$tgts" | grep -qw AArch64    && check_true "targets-built has AArch64" 0    || check_true "targets-built has AArch64" 1
echo "$tgts" | grep -qw RISCV      && check_true "targets-built has RISCV" 0      || check_true "targets-built has RISCV" 1
echo "$tgts" | grep -qw LoongArch  && check_true "targets-built has LoongArch" 0  || check_true "targets-built has LoongArch" 1

# -------------------------------------------------------------------------
# 2. --version / --help sweep: every tool self-reports LLVM 22 and its metadata
# -------------------------------------------------------------------------
echo "[--version / --help]"
check_version llvm-as
check_version llvm-dis
check_version llvm-link
check_version llvm-nm
check_version llvm-objdump
check_version llvm-ar
check_version opt
check_version llc
check_version lli

opt --print-passes 2>/dev/null > passes.txt
if grep -qw mem2reg passes.txt && grep -qw instcombine passes.txt; then
    check_true "opt --print-passes lists mem2reg + instcombine" 0
else
    check_true "opt --print-passes lists mem2reg + instcombine" 1
fi

llc --version 2>/dev/null > llctgt.txt
if grep -qw aarch64 llctgt.txt && grep -qw riscv64 llctgt.txt; then
    check_true "llc --version lists aarch64 + riscv64 targets" 0
else
    check_true "llc --version lists aarch64 + riscv64 targets" 1
fi

FileCheck --help >/dev/null 2>&1
check_true "FileCheck --help exits 0" $?

# -------------------------------------------------------------------------
# 3. llvm-as / llvm-dis: IR <-> bitcode roundtrip + empty module
# -------------------------------------------------------------------------
echo "[llvm-as / llvm-dis]"
llvm-as "$SRC/hello.ll" -o hello.bc 2>/dev/null
magic=$(od -An -tx1 -N4 hello.bc 2>/dev/null | tr -d ' \n')
check_eq "llvm-as emits bitcode magic (BC C0 DE)" "4243c0de" "$magic"

llvm-dis hello.bc -o hello_rt.ll 2>/dev/null
grep -q '@main' hello_rt.ll && check_true "llvm-dis disassembles (has @main)" 0 || check_true "llvm-dis disassembles (has @main)" 1

llvm-as hello_rt.ll -o /dev/null 2>/dev/null
check_true "llvm-dis output re-assembles (roundtrip stable)" $?

llvm-as "$SRC/empty.ll" -o empty.bc 2>/dev/null
emagic=$(od -An -tx1 -N4 empty.bc 2>/dev/null | tr -d ' \n')
check_eq "llvm-as accepts empty module (valid bitcode)" "4243c0de" "$emagic"

# -------------------------------------------------------------------------
# 4. lli: JIT execution
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

# Large IR: a 1000-instruction add chain evaluating to 1000; exit code truncates to 232.
awk 'BEGIN {
    print "define i32 @main() {"; print "entry:";
    print "  %v0 = add i32 0, 1";
    for (i = 1; i < 1000; i++) printf "  %%v%d = add i32 %%v%d, 1\n", i, i - 1;
    print "  ret i32 %v999"; print "}"
}' > big.ll
lli big.ll >/dev/null 2>&1
check_eq "lli large IR (1000-add chain): exit code 1000 & 255" "232" "$?"

# -------------------------------------------------------------------------
# 5. llc: native asm / object + cross-target object codegen (all four targets)
# -------------------------------------------------------------------------
echo "[llc]"
llc "$SRC/hello.ll" -o hello.s 2>/dev/null
if grep -q 'main' hello.s && grep -qi 'globl' hello.s; then
    check_true "llc emits native asm (.globl main)" 0
else
    check_true "llc emits native asm (.globl main)" 1
fi

llc -filetype=obj "$SRC/hello.ll" -o hello.o 2>/dev/null
omagic=$(od -An -tx1 -N4 hello.o 2>/dev/null | tr -d ' \n')
check_eq "llc emits native ELF object" "7f454c46" "$omagic"

nm_out=$(llvm-nm hello.o 2>/dev/null)
echo "$nm_out" | grep -q 'T main' && check_true "llvm-nm lists 'T main'" 0 || check_true "llvm-nm lists 'T main'" 1

od_out=$(llvm-objdump -d hello.o 2>/dev/null)
echo "$od_out" | grep -q 'main' && check_true "llvm-objdump disassembles main" 0 || check_true "llvm-objdump disassembles main" 1

llc -filetype=obj -mtriple=x86_64 "$SRC/hello.ll" -o cross_x86.o 2>/dev/null
check_eq "llc cross-compile x86_64 (ELF e_machine=62)" "62" "$(elf_machine cross_x86.o)"

llc -filetype=obj -mtriple=aarch64 "$SRC/hello.ll" -o cross_aa.o 2>/dev/null
check_eq "llc cross-compile aarch64 (ELF e_machine=183)" "183" "$(elf_machine cross_aa.o)"

llc -filetype=obj -mtriple=riscv64 "$SRC/hello.ll" -o cross_rv.o 2>/dev/null
check_eq "llc cross-compile riscv64 (ELF e_machine=243)" "243" "$(elf_machine cross_rv.o)"

llc -filetype=obj -mtriple=loongarch64 "$SRC/hello.ll" -o cross_la.o 2>/dev/null
check_eq "llc cross-compile loongarch64 (ELF e_machine=258)" "258" "$(elf_machine cross_la.o)"

# -------------------------------------------------------------------------
# 6. opt: mem2reg / -O0..-O3 / default<O2> / instcombine / inline / bitcode pipeline
# -------------------------------------------------------------------------
echo "[opt]"
check_eq "mem2reg fixture has 2 allocas" "2" "$(count_alloca "$SRC/mem2reg.ll")"

opt -passes=mem2reg -S "$SRC/mem2reg.ll" -o m2r.ll 2>/dev/null
check_eq "opt -passes=mem2reg eliminates allocas" "0" "$(count_alloca m2r.ll)"

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

opt -passes=instcombine -S "$SRC/instcombine.ll" -o ic.ll 2>/dev/null
if [ "$(grep -c ' mul ' ic.ll)" = "0" ] && [ "$(grep -c ' add ' ic.ll)" = "0" ] && grep -q 'ret i32 %x' ic.ll; then
    check_true "opt -passes=instcombine folds (x*1)+0 to x" 0
else
    check_true "opt -passes=instcombine folds (x*1)+0 to x" 1
fi

opt -passes=inline -S "$SRC/inline.ll" -o inl.ll 2>/dev/null
check_eq "opt -passes=inline removes the call site" "0" "$(grep -c 'call i32 @callee' inl.ll)"

llvm-as "$SRC/mem2reg.ll" -o m2r_in.bc 2>/dev/null
opt -passes=mem2reg m2r_in.bc -o m2r_out.bc 2>/dev/null
llvm-dis m2r_out.bc -o m2r_out.ll 2>/dev/null
check_eq "opt bitcode pipeline (as|opt|dis) no alloca" "0" "$(count_alloca m2r_out.ll)"

# -------------------------------------------------------------------------
# 7. llvm-link: module merge
# -------------------------------------------------------------------------
echo "[llvm-link]"
llvm-link "$SRC/link_a.ll" "$SRC/link_b.ll" -S -o linked.ll 2>/dev/null
if grep -q '@funcA' linked.ll && grep -q '@funcB' linked.ll; then
    check_true "llvm-link merges both modules (@funcA + @funcB)" 0
else
    check_true "llvm-link merges both modules (@funcA + @funcB)" 1
fi

# -------------------------------------------------------------------------
# 8. llvm-ar: static archive create / list / symbol index / extract
# -------------------------------------------------------------------------
echo "[llvm-ar]"
llc -filetype=obj "$SRC/link_a.ll" -o arA.o 2>/dev/null
llc -filetype=obj "$SRC/link_b.ll" -o arB.o 2>/dev/null
rm -f lib.a
llvm-ar rcs lib.a arA.o arB.o 2>/dev/null
armagic=$(od -An -c -N8 lib.a 2>/dev/null | tr -d ' \n')
check_eq "llvm-ar rcs creates archive (! < a r c h > magic)" '!<arch>\n' "$armagic"

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
# 9. FileCheck: positive + negative discrimination
# -------------------------------------------------------------------------
echo "[FileCheck]"
llvm-dis hello.bc -o hello_check_in.ll 2>/dev/null
FileCheck "$SRC/hello.check" < hello_check_in.ll >/dev/null 2>&1
check_true "FileCheck positive match" $?

FileCheck "$SRC/bad.check" < hello_check_in.ll >/dev/null 2>&1
check_false "FileCheck negative (must fail on absent pattern)" $?

# -------------------------------------------------------------------------
# 10. clang: C front-end (IR + native exe + -std dialects)
# -------------------------------------------------------------------------
echo "[clang]"
cver=$(clang --version 2>/dev/null | head -1)
echo "$cver" | grep -q "version $EXP_MAJOR\." && check_true "clang --version is $EXP_MAJOR.x" 0 || check_true "clang --version is $EXP_MAJOR.x" 1

clang -S -emit-llvm -O0 "$SRC/hello.c" -o hello_c.ll 2>/dev/null
if grep -q 'define' hello_c.ll && grep -q '@main' hello_c.ll && grep -q '@printf' hello_c.ll; then
    check_true "clang -emit-llvm: C -> IR (@main + @printf)" 0
else
    check_true "clang -emit-llvm: C -> IR (@main + @printf)" 1
fi

clang -std=c11 -S -emit-llvm "$SRC/hello.c" -o hello_c11.ll 2>/dev/null
if [ $? -eq 0 ] && grep -q '@main' hello_c11.ll; then
    check_true "clang -std=c11: C -> IR" 0
else
    check_true "clang -std=c11: C -> IR" 1
fi

clang -std=gnu17 -S -emit-llvm "$SRC/hello.c" -o hello_g17.ll 2>/dev/null
if [ $? -eq 0 ] && grep -q '@main' hello_g17.ll; then
    check_true "clang -std=gnu17: C -> IR" 0
else
    check_true "clang -std=gnu17: C -> IR" 1
fi

clang "$SRC/hello.c" -o hello_bin 2>/dev/null
out=$(./hello_bin 2>/dev/null)
check_eq "clang C -> exe: compile+link+run" "CLANG22 OK" "$out"

clang -O2 "$SRC/math.c" -lm -o math_bin 2>/dev/null
out=$(./math_bin 2>/dev/null)
check_eq "clang -O2 -lm: sqrt(2)" "SQRT=1.4142" "$out"

# -------------------------------------------------------------------------
# 11. clang++: C++ front-end (name mangling + templates + STL)
# -------------------------------------------------------------------------
echo "[clang++]"
cxxver=$(clang++ --version 2>/dev/null | head -1)
echo "$cxxver" | grep -q "version $EXP_MAJOR\." && check_true "clang++ --version is $EXP_MAJOR.x" 0 || check_true "clang++ --version is $EXP_MAJOR.x" 1

clang++ -std=c++17 -S -emit-llvm -O0 "$SRC/hello.cpp" -o hello_cpp.ll 2>/dev/null
grep -q '_ZN7Counter' hello_cpp.ll && check_true "clang++ -emit-llvm: C++ -> IR (Itanium name mangling)" 0 || check_true "clang++ -emit-llvm: C++ -> IR (Itanium name mangling)" 1

clang++ -std=c++17 "$SRC/hello.cpp" -o hello_cpp 2>/dev/null
out=$(./hello_cpp 2>/dev/null)
check_eq "clang++ C++ -> exe: templates + STL + run" "CPP22 SUM=15 CNT=5" "$out"

# -------------------------------------------------------------------------
# 12. lld: LLVM linker
# -------------------------------------------------------------------------
echo "[lld]"
lver=$(ld.lld --version 2>/dev/null)
echo "$lver" | grep -q "LLD $EXP_MAJOR\." && check_true "ld.lld --version is $EXP_MAJOR.x" 0 || check_true "ld.lld --version is $EXP_MAJOR.x" 1

clang -fuse-ld=lld "$SRC/hello.c" -o hello_lld 2>/dev/null
out=$(./hello_lld 2>/dev/null)
check_eq "clang -fuse-ld=lld: link+run" "CLANG22 OK" "$out"

# -------------------------------------------------------------------------
# 13. error handling: malformed IR / unknown pass / missing input must fail
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
# 14. integration pipelines
# -------------------------------------------------------------------------
echo "[pipelines]"
# clang front-end -> opt -O2 -> llc object -> lld link -> run: the full toolchain.
# llc defaults to a static reloc model; clang links PIE, so ask llc for a PIC object.
clang -S -emit-llvm -O0 "$SRC/hello.c" -o pipe.ll 2>/dev/null
opt -O2 -S pipe.ll -o pipe_opt.ll 2>/dev/null
llc -relocation-model=pic -filetype=obj pipe_opt.ll -o pipe.o 2>/dev/null
clang -fuse-ld=lld pipe.o -o pipe_bin 2>/dev/null
out=$(./pipe_bin 2>/dev/null)
check_eq "pipeline clang|opt|llc|lld -> run" "CLANG22 OK" "$out"

# opt-optimised IR fed straight back into the JIT still computes the right answer.
opt -O2 -S "$SRC/loop.ll" -o loop_opt.ll 2>/dev/null
out=$(lli loop_opt.ll 2>/dev/null)
check_eq "pipeline opt -O2 loop.ll | lli -> SUM" "SUM=5050" "$out"

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
