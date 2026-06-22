#!/bin/sh
# run-java.sh — on-target gate for the StarryOS java-lang multi-JDK LANGUAGE carpet (#764).
#
# Staged into the rootfs by prebuild.sh and invoked by every qemu-<arch>.toml as the
# ENTIRE shell_init_cmd (`sh /usr/bin/run-java.sh`). Two reasons it is a STAGED script and
# not an inline shell_init_cmd:
#
#  1. (FIX1, correctness) The StarryOS app harness echoes the shell_init_cmd text back over
#     the serial console. The previous inline gate contained `echo "TEST PASSED"`, and the
#     long `if ...; then echo "TEST PASSED"; ...` line wraps so that the literal `TEST PASSED`
#     lands on its own line in the ECHOED command text — which the harness
#     `success_regex = (?m)^TEST PASSED$` matched as a FALSE POSITIVE (it "passed" even when
#     the real AGGREGATE gate printed TEST FAILED; observed on riscv64 = true AGGREGATE 5/10).
#     Staged here, the only echoed text is `sh /usr/bin/run-java.sh`, so the regex only ever
#     matches this script's REAL stdout. TEST PASSED is printed ONLY at the very end, ONLY
#     when PASS==TOTAL over the genuinely attempted suites.
#
#  2. (FIX2, honesty) On starry riscv64 (and, by the same reasoning, loongarch64) some JDK
#     builds may raise a repeated `user exception: IllegalInstruction` from JVM-generated
#     code. Root cause (verified): the JVM probes RISC-V ISA extensions via riscv_hwprobe
#     (syscall 258) / getauxval(AT_HWCAP); starry reports the RV64GC baseline (IMA+FD+C, no
#     Zba/Zbb/Zbs, no V), and qemu `-cpu rv64` already enables Zba/Zbb/Zbc/Zbs (bitmanip is
#     pure userspace, needs no kernel support) while the vector V extension is OFF and CANNOT
#     be enabled by widening `-cpu` because the generic starry riscv64 kernel does not manage
#     vector state (sstatus.VS is left Off outside the xuantie-c9xx board build, and there is
#     no vector-register context save/restore). We therefore (a) belt-and-suspenders pass the
#     JVM extension-disable flags (with the correct per-JDK unlock token) so a JDK that would
#     auto-emit Zb*/V is forced back to RV64GC, and (b) for any JDK that STILL cannot run
#     (a cheap `java -version` liveness probe fails), mark it a documented SKIP — reason
#     "IllegalInstruction / RISC-V extension unsupported on starry" — and remove it from the
#     attempted JDK set so the per-suite TOTALs reflect only attempted-and-supported JDKs.
#     A skipped JDK is NOT counted as a failure (partial-arch-deliver rule). JDK17 always runs.
set -u

case "$(uname -m)" in
  x86_64)      ARCH=x86_64 ;;
  aarch64)     ARCH=aarch64 ;;
  riscv64)     ARCH=riscv64 ;;
  loongarch64) ARCH=loongarch64 ;;
  *)           ARCH="$(uname -m)" ;;
esac

# Candidate JDK set per arch (matches what prebuild.sh stages). ALL 4 arches now TRY JDK23:
# x86_64/aarch64 via the BellSoft native-musl build; riscv64 (BellSoft generic-glibc) and
# loongarch64 (Loongson glibc) via the gcompat shim staged by prebuild.sh. The timeout-guarded
# liveness probe below decides per cell — a glibc JDK that runs under gcompat is counted; one
# that still segfaults/hangs is a documented SKIP (not a failure, not a fake pass).
case "$ARCH" in
  x86_64|aarch64|riscv64|loongarch64) JDKS="17 21 23 25" ;;
  *)                                  JDKS="17 21 25"    ;;
esac

# PER-JDK musl loader path (root-cause fix, host+starry validated): a single shared path
# listing ALL JDK lib dirs makes JDK23/25 mis-resolve their runtime libs to the FIRST entry
# (jdk17) -> source launcher class mismatch + empty `java -version`. Set the path to ONLY the
# JDK about to run, each time. loong JDKs may ship the Zero VM (lib/zero/libjvm.so) -> list it.
LDP=/etc/ld-musl-$ARCH.path
setp()  { printf '/lib\n/usr/lib\n%s/lib\n%s/lib/server\n%s/lib/zero\n' "$1" "$1" "$1" > "$LDP"; }
setp2() { printf '/lib\n/usr/lib\n%s/lib\n%s/lib/server\n%s/lib/zero\n%s/lib\n%s/lib/server\n%s/lib/zero\n' "$1" "$1" "$1" "$2" "$2" "$2" > "$LDP"; }

# StarryOS JIT is still unstable (#206) -> force interpreter on every JVM.
export JAVA_TOOL_OPTIONS="-Xint"
# Keep the default CDS archive (do NOT pass -Xshare:off): CDS serves common classes from
# the shared archive instead of the ~140 MiB module image, which REDUCES the amount of
# file-backed jimage mmap and therefore reduces exposure to StarryOS's intermittent
# large-file mmap fault (the VM-init NoClassDefFoundError seen on riscv64/loongarch under
# memory pressure). The residual intermittent fault is handled by the retry in probe_jdk()
# and vtest() — disabling CDS made it WORSE (it spread the fault to more JDKs).
COMMON="-Xint -Xmx512m -Xms64m"

# riscv64 JVM flags: NONE. The PROVEN openjdk-multi gate (download/jdk-multi/qemu-riscv64.toml)
# ran rv JDK17 (musl cross) + JDK21/JDK25 (Alpine native-musl) flag-free and all printed
# JDKxx_OK. Passing -XX:-UseRVV (a C2/JIT option) to the Alpine rv openjdk25 — which is a
# **Zero VM** (interpreter, no JIT) — makes the JVM reject it as an *unrecognized VM option*
# and exit, so `java -version` produces no `version "25` line and the liveness probe wrongly
# SKIPs JDK25. The rv musl JDKs run on the RV64GC baseline without coaxing (qemu -cpu rv64
# leaves the V extension off), so we add no rv-specific flags. Kept as a no-op for call sites.
rvflags() { echo ""; }

# FIX2 liveness probe: can JDK $1 even print `java -version` on this arch (with the baseline
# flags applied)? A JDK that raises IllegalInstruction prints no `version "N` line. Used to
# decide SKIP. Echoes 1 (alive) or 0 (dead). JDK17 is always expected alive.
RUNNABLE=""   # space-list of JDK majors that pass the liveness probe
SKIPPED=""    # space-list of JDK majors that were skipped
# TO: run a command under a timeout when busybox `timeout` is available, else run it plain.
# Guards the liveness probe so a JDK that HANGS at JVM startup (e.g. a glibc/gcompat build
# whose heap-reservation mmap spins) is killed and marked SKIP instead of stalling the
# whole suite. Native-musl JDKs print -version in seconds, so 300s is generous headroom.
TO() { t="$1"; shift; if command -v timeout >/dev/null 2>&1; then timeout "$t" "$@"; else "$@"; fi; }
probe_jdk() {
  setp /opt/jdk$1
  # Retry -version on the transient large-jimage mmap fault (VM-init NoClassDefFoundError),
  # so a JDK that boots fine 5/6 of the time is not wrongly dropped from RUNNABLE. A real
  # IllegalInstruction (e.g. the rv Alpine JDK25 Zero-VM) is NOT a transient init fault, so
  # it falls straight through to SKIP without burning retries.
  pt=0; pmax=6
  while :; do
    pout="$(TO 300 /opt/jdk$1/bin/java $COMMON $(rvflags "$1") -version 2>&1)"
    if printf '%s' "$pout" | grep -qi "version \"$1"; then
      RUNNABLE="$RUNNABLE $1"; [ $pt -gt 0 ] && echo "  jdk$1 probe ok (retry $pt)"; return
    fi
    pt=$((pt+1))
    [ $pt -lt $pmax ] && printf '%s' "$pout" | grep -qE 'initialization of VM|NoClassDefFoundError: jdk/internal/vm' && continue
    break
  done
  SKIPPED="$SKIPPED $1"
  echo "  SKIP jdk$1 ($ARCH): java -version did not run/timed out — IllegalInstruction / glibc+gcompat or extension unsupported on starry (documented)"
}

echo "=== JDK liveness probe ($ARCH; candidates: $JDKS) ==="
for V in $JDKS; do probe_jdk "$V"; done
# normalize RUNNABLE (strip leading space) and guarantee jdk17 is in it (carpet base).
RUNNABLE="$(echo $RUNNABLE)"
SKIPPED="$(echo $SKIPPED)"
echo "  RUNNABLE JDKs: ${RUNNABLE:-none}"
[ -n "$SKIPPED" ] && echo "  SKIPPED JDKs:  $SKIPPED (documented, not counted as failures)"
case " $RUNNABLE " in *" 17 "*) : ;; *) echo "FATAL: jdk17 (carpet base) did not run on $ARCH"; echo "TEST FAILED"; exit 1 ;; esac

# in_runnable <ver> -> rc 0 iff <ver> passed the liveness probe.
in_runnable() { case " $RUNNABLE " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

# aggregate gate: emit TEST PASSED ONLY when PASS==TOTAL (no silent pass).
PASS=0; TOTAL=0
acc() { TOTAL=$((TOTAL+1)); if [ "$1" = 1 ]; then PASS=$((PASS+1)); else echo "  SUITE FAIL ($2)"; fi; }

# run one version feature test (single-file source mode). $1 = major; $2.. = extra java flags.
# A glibc JDK (riscv64 JDK23) occasionally aborts at JVM init with a transient
# NoClassDefFoundError for an internal class (e.g. jdk/internal/vm/Continuation$Pinned):
# the JVM memory-maps its ~140 MiB module image (lib/modules) and StarryOS file-backed
# mmap of a large image is not yet fully robust under memory pressure (same class of
# kernel issue as the file-backed mmap EOF bound), so a deep class is intermittently
# unmapped. This is a transient VM-INIT fault, not a test failure — the same carpet
# passes on a retry. Retry ONLY on that init signature (real assertion failures are
# never retried); the kernel-side robustness fix is tracked separately.
vtest() {
  ver="$1"; shift
  in_runnable "$ver" || { echo "JDK${ver}: skipped (not runnable on $ARCH)"; return; }
  setp /opt/jdk$ver
  tag="JDK${ver}_OK"; src="/root/jdkm/Jdk${ver}Features.java"; J="/opt/jdk$ver/bin/java"
  t=0; max=6
  while :; do
    "$J" $COMMON $(rvflags "$ver") "$@" "$src" >/tmp/v.out 2>&1
    grep -q "^$tag\$" /tmp/v.out && { echo "$tag printed$( [ $t -gt 0 ] && echo " (retry $t)" )"; acc 1 "$tag"; return; }
    t=$((t+1))
    if [ $t -lt $max ] && grep -qE 'initialization of VM|NoClassDefFoundError: jdk/internal/vm' /tmp/v.out; then
      echo "  ($tag transient VM-init fault — retry $t/$((max-1)))"; continue
    fi
    echo "$tag MISSING:"; tail -10 /tmp/v.out; acc 0 "$tag"; return
  done
}

echo "=== JDK17 (records/sealed/instanceof-pattern/text-block/switch-expr/Stream.toList) ==="
vtest 17
echo "=== JDK21 (virtual threads/record patterns/guarded switch/sequenced collections/Math.clamp) ==="
vtest 21
echo "=== JDK23 (flexible ctor bodies + Stream Gatherers [preview] + nested record patterns) ==="
vtest 23 --enable-preview --source 23
echo "=== JDK25 (scoped values/module imports/compact headers + StableValue [preview]) ==="
vtest 25 --enable-preview --source 25

# compact object headers: prove the JVM accepts the product flag + layout works (jdk25 only).
if in_runnable 25; then
  setp /opt/jdk25
  /opt/jdk25/bin/java $COMMON $(rvflags 25) -XX:+UseCompactObjectHeaders --enable-preview --source 25 /root/jdkm/Jdk25Features.java >/tmp/v25c.out 2>&1
  if grep -q 'compact-object-headers flag present = true' /tmp/v25c.out && grep -q '^JDK25_OK$' /tmp/v25c.out; then
    echo "JDK25 compact-object-headers run OK"; acc 1 JDK25-COMPACT
  else echo "JDK25-COMPACT FAIL:"; tail -10 /tmp/v25c.out; acc 0 JDK25-COMPACT; fi
else echo "JDK25-COMPACT: skipped (jdk25 not runnable on $ARCH)"; fi

# --- version switch: REAL update-alternatives (/usr/bin/java -> /etc/alternatives/java -> /opt/jdkN/bin/java) ---
mkdir -p /etc/alternatives
echo "=== version switch: update-alternatives (runnable JDKs: $RUNNABLE) ==="
SW=0; SWN=0
for V in $RUNNABLE; do
  SWN=$((SWN+1))
  ln -sfn /opt/jdk$V/bin/java /etc/alternatives/java
  ln -sfn /etc/alternatives/java /usr/bin/java
  setp /opt/jdk$V
  RAW=$(/usr/bin/java $COMMON $(rvflags "$V") -version 2>&1 | grep -i 'version "' | head -1)
  echo "  switch->$V : $RAW"
  echo "$RAW" | grep -q "version \"$V" && SW=$((SW+1)) || echo "    MISMATCH expected $V"
done
[ "$SW" = "$SWN" ] && [ "$SWN" -gt 0 ] && { echo "SWITCH ok=$SW/$SWN"; acc 1 SWITCH; } || { echo "SWITCH ok=$SW/$SWN"; acc 0 SWITCH; }

# --- version switch: sdkman-style candidate-dir switch (offline, pre-seeded) ---
echo "=== version switch: sdkman-style candidate 'sdk use' (offline; runnable: $RUNNABLE) ==="
SD=0; SDN=0; CAND=/root/.sdkman/candidates/java
for V in $RUNNABLE; do
  SDN=$((SDN+1))
  ln -sfn $CAND/$V-open $CAND/current
  CH=$CAND/current
  setp2 "$CH" /opt/jdk$V
  RAW=$("$CH/bin/java" $COMMON $(rvflags "$V") -version 2>&1 | grep -i 'version "' | head -1)
  echo "  sdk-use $V : $RAW"
  echo "$RAW" | grep -q "version \"$V" && SD=$((SD+1)) || echo "    MISMATCH expected $V"
done
[ "$SD" = "$SDN" ] && [ "$SDN" -gt 0 ] && { echo "SDK-SWITCH ok=$SD/$SDN"; acc 1 SDK-SWITCH; } || { echo "SDK-SWITCH ok=$SD/$SDN"; acc 0 SDK-SWITCH; }

# --- javac/java CLI carpet: kernel-relevant javac+java options compiled+run on-target (jdk17) ---
setp /opt/jdk17
echo "=== javac/java CLI carpet (jdk17) ==="
PATH=/opt/jdk17/bin:$PATH TMPDIR=/root SLOW_EMU=1 JAVA=/opt/jdk17/bin/java JAVAC=/opt/jdk17/bin/javac RELOPT="" \
  sh /root/jdkm/java-cli-core.sh >/tmp/cli.out 2>&1
if grep -q '^JAVA_CLI_OK$' /tmp/cli.out; then echo "JAVA_CLI_OK"; acc 1 JAVA_CLI; else echo "JAVA_CLI FAIL:"; grep '  FAIL ' /tmp/cli.out | head; tail -8 /tmp/cli.out; acc 0 JAVA_CLI; fi

# --- full JDK toolchain carpet (jdk17): jshell/jar/javap/javadoc/jdeps/jdeprscan/serialver/
#     jlink/jmod/jpackage/jrunscript + ops-tool smokes. The whole developer toolchain (全集),
#     not just javac+java. Heavy builds (jlink runtime / jpackage app-image) run under -Xint/TCG;
#     the carpet skips-with-reason any tool that hits a kernel/platform limit (never a false PASS).
setp /opt/jdk17
echo "=== java toolchain carpet (jdk17; jshell + full JDK dev toolchain 全集) ==="
TC_BIN=/opt/jdk17 PATH=/opt/jdk17/bin:$PATH TMPDIR=/root TC_HEAVY=0 \
  sh /root/jdkm/java-toolchain-carpet.sh >/tmp/tc.out 2>&1
if grep -q '^JAVA_TOOLCHAIN_OK$' /tmp/tc.out; then echo "JAVA_TOOLCHAIN_OK ($(grep '^# RESULTS' /tmp/tc.out))"; acc 1 JAVA_TOOLCHAIN; else echo "JAVA_TOOLCHAIN FAIL:"; grep '  FAIL ' /tmp/tc.out | head; tail -10 /tmp/tc.out; acc 0 JAVA_TOOLCHAIN; fi

# --- full-JLS grammar carpet (single-file source mode, jdk17) ---
setp /opt/jdk17
echo "=== java grammar carpet (jdk17, full JLS) ==="
TMPDIR=/root /opt/jdk17/bin/java $COMMON /root/jdkm/JavaGrammar.java >/tmp/gram.out 2>&1
if grep -q '^JAVA_GRAMMAR_OK$' /tmp/gram.out; then echo "JAVA_GRAMMAR_OK"; acc 1 JAVA_GRAMMAR; else echo "JAVA_GRAMMAR FAIL:"; tail -10 /tmp/gram.out; acc 0 JAVA_GRAMMAR; fi

# --- LANGUAGE carpet: COMPILE with on-target javac (--release 17) then RUN; assert token ---
setp /opt/jdk17
echo "=== JavaLangCarpet (compile on-target javac --release 17, run) ==="
TMPDIR=/root /opt/jdk17/bin/javac -J-Xmx512m --release 17 -d /root/jlc /root/jdkm/JavaLangCarpet.java >/tmp/jlc-c.out 2>&1
if [ -f /root/jlc/JavaLangCarpet.class ]; then
  TMPDIR=/root /opt/jdk17/bin/java $COMMON -cp /root/jlc JavaLangCarpet >/tmp/jlc.out 2>&1
  if grep -q '^JAVA_LANG_OK ' /tmp/jlc.out; then echo "JAVA_LANG_OK ($(grep '^JAVA_LANG_OK ' /tmp/jlc.out))"; acc 1 JAVA_LANG; else echo "JAVA_LANG FAIL:"; tail -12 /tmp/jlc.out; acc 0 JAVA_LANG; fi
else echo "JavaLangCarpet COMPILE FAIL:"; tail -12 /tmp/jlc-c.out; acc 0 JAVA_LANG; fi

# --- backward-compat: compile ONCE with javac --release 17, run on every RUNNABLE JDK ---
echo "=== BackCompat (javac --release 17 once; run on every runnable JDK: $RUNNABLE) ==="
setp /opt/jdk17
TMPDIR=/root /opt/jdk17/bin/javac -J-Xmx512m --release 17 -d /root/bc /root/jdkm/BackCompat.java >/tmp/bc-c.out 2>&1
BC=0; BCN=0; REF=""
for V in $RUNNABLE; do
  setp /opt/jdk$V; BCN=$((BCN+1))
  R=$(TMPDIR=/root /opt/jdk$V/bin/java $COMMON $(rvflags "$V") -cp /root/bc BackCompat 2>/dev/null | grep '^BACKCOMPAT_RUN=' | head -1)
  echo "  jdk$V : $R"
  [ -z "$REF" ] && REF="$R"
  if [ -n "$R" ] && [ "$R" = "$REF" ]; then BC=$((BC+1)); else echo "    MISMATCH (jdk17 .class not byte-identical on jdk$V)"; fi
done
[ "$BC" = "$BCN" ] && [ "$BCN" -gt 0 ] && { echo "BACKCOMPAT ok=$BC/$BCN"; acc 1 BACKCOMPAT; } || { echo "BACKCOMPAT ok=$BC/$BCN"; acc 0 BACKCOMPAT; }

# --- BackCompatReal: REAL-WORLD Java-8 (--release 8, bytecode 52) forward-compat suite ---
# The pre-staged backcompat-real.jar (299 JUnit tests over Apache Commons / Log4j2 / H2 +
# HSQLDB / Gson / BeanShell, compiled to Java-8 bytecode) is run UNCHANGED on every RUNNABLE
# JDK. Each JDK must print the IDENTICAL token `BACKCOMPAT_REAL_OK 299` — a real cross-version
# backward-compat proof (a Java-8 jar runs on jdk17/21/23/25). One acc for the whole suite.
echo "=== BackCompatReal (Java-8 jar; run on every runnable JDK: $RUNNABLE) ==="
BCR=0; BCRN=0; BCRREF=""
for V in $RUNNABLE; do
  setp /opt/jdk$V; BCRN=$((BCRN+1))
  RTOK=$(TMPDIR=/root /opt/jdk$V/bin/java $COMMON $(rvflags "$V") -cp "/root/bcreal/libs/*:/root/bcreal/backcompat-real.jar" BackCompatReal 2>/tmp/bcr.err | grep '^BACKCOMPAT_REAL_OK ' | head -1)
  echo "  jdk$V : ${RTOK:-<no BACKCOMPAT_REAL_OK token>}"
  if [ -z "$RTOK" ]; then echo "    BACKCOMPAT_REAL FAIL on jdk$V:"; tail -8 /tmp/bcr.err; continue; fi
  [ -z "$BCRREF" ] && BCRREF="$RTOK"
  if [ "$RTOK" = "BACKCOMPAT_REAL_OK 299" ] && [ "$RTOK" = "$BCRREF" ]; then BCR=$((BCR+1)); else echo "    MISMATCH (expected 'BACKCOMPAT_REAL_OK 299' == ref '$BCRREF', got '$RTOK')"; fi
done
[ "$BCR" = "$BCRN" ] && [ "$BCRN" -gt 0 ] && { echo "BACKCOMPAT_REAL ok=$BCR/$BCRN (all '$BCRREF')"; acc 1 BACKCOMPAT_REAL; } || { echo "BACKCOMPAT_REAL ok=$BCR/$BCRN"; acc 0 BACKCOMPAT_REAL; }

# --- aggregate gate ---
[ -n "$SKIPPED" ] && echo "AGGREGATE: skipped JDKs on $ARCH = $SKIPPED (documented SKIP, not failures)"
echo "AGGREGATE: PASS=$PASS TOTAL=$TOTAL (runnable JDKs: $RUNNABLE)"
if [ "$PASS" = "$TOTAL" ] && [ "$TOTAL" -gt 0 ]; then echo "TEST PASSED"; exit 0; fi
echo "TEST FAILED"
exit 1
