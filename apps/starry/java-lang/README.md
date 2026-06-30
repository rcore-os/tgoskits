# Starry java-lang App — multi-JDK (OpenJDK 17 / 21 / 23 / 25) `javac` · `java` carpet

This app runs an industrial, carpet-coverage Java **language + compiler + runtime**
test suite inside StarryOS QEMU across `x86_64 / aarch64 / riscv64 / loongarch64`.
**aarch64 / riscv64 / loongarch64 run the full JDK 17 / 21 / 23 / 25 set today** (each
`AGGREGATE PASS=13/13` + `TEST PASSED`). **x86_64 also passes 4/4 — but only on a
kernel with its two companion StarryOS fixes #1366 + #1367 applied** (verified locally
on such a kernel: `segv34=0`, `AGGREGATE PASS=13/13`); it ships a `qemu-x86_64.toml`
and is **Blocked-by #1366 + #1367** until they merge (on bare `dev` x86_64 still
SIGSEGV-storms — see the **x86_64 status** note). With those two merged the carpet
reaches **4 arch × 4 JDK = 16/16**. It is the #764 item:

> `jdk17+ (openjdk 17 21 23 25 update-alternatives): javac · java`

Unlike the `kotlin-lang` case (which only *runs* a precompiled jar and can use a
~60 MB JRE), this case **compiles on-target** (`javac`), so it stages the **FULL
JDK** (`bin/javac` + `lib` + `jmods`) for every version and exercises both the
compiler and the runtime end to end.

## What it covers

For each staged JDK (per-arch set below) the aggregate gate asserts, with no
silent pass (`TEST PASSED` is emitted only when `PASS == TOTAL`):

- **Per-version language + stdlib features** (`Jdk{17,21,23,25}Features.java`, run
  via single-file source mode, each red-lines `Runtime.version().feature()`):
  - JDK17 — records, sealed types, `instanceof` patterns, text blocks, switch
    expressions + `yield`, `Stream.toList()`.
  - JDK21 — virtual threads (`Thread.ofVirtual`, `newVirtualThreadPerTaskExecutor`
    fan-out of 1000 tasks), record patterns + guarded switch, sequenced
    collections, `Math.clamp`.
  - JDK23 — flexible constructor bodies (statements before `super()`), Stream
    Gatherers (`windowFixed`/`fold`), nested record patterns (preview, gated by
    `--enable-preview --source 23`).
  - JDK25 — scoped values, module import declarations (`import module java.base`),
    compact object headers (`-XX:+UseCompactObjectHeaders` product flag), stable
    values (preview).
- **`javac` + `java` CLI carpet** (`java-cli-core.sh`) — the kernel-relevant subset
  of the launcher/compiler option surface, *compiled and run on the target JDK*:
  `javac` `--help`/`-d`/`-cp`/`--release`/`-encoding`/`-g`/`-Xlint`; `java`
  `--help`/`-X`/`-cp`/`-D`/`-ea`/`-jar`/`--source`/`--dry-run`/`--list-modules`/
  `--describe-module`/`-verbose:class`/`-XshowSettings`/exit-code propagation/
  `JAVA_TOOL_OPTIONS`/`CLASSPATH`.
- **JDK developer-toolchain carpet** (`java-toolchain-carpet.sh`) — `jshell` REPL +
  the JDK dev toolchain (`jar`/`javap`/`javadoc`/`jdeps`/`jdeprscan`/`serialver`/
  `jlink`/`jmod`/`jpackage`/`jrunscript`, each `--help`/`--version` + a real-function
  assertion) plus `jcmd`/`jps`/`jstat`/`jstack`/`jmap`/`jinfo`/`jfr`/`keytool`/
  `jarsigner`/`jdb` ops smokes → `JAVA_TOOLCHAIN_OK` (one of the 13 aggregate suites).
- **Full-JLS grammar carpet** (`JavaGrammar.java`) — `JAVA_GRAMMAR_OK`.
- **Language carpet** (`JavaLangCarpet.java`, 316 `chk()` assertions + 55 `golden()`
  values; some `chk()` run inside loops so the printed `JAVA_LANG_OK <n>` is higher) —
  **compiled on-target** with `javac --release 17`, then run, asserting
  `JAVA_LANG_OK`. Doc-grounded against the JLS + `java.base`: every primitive +
  boxing/widening/narrowing, every operator, all control flow, classes/interfaces/
  enums/records/sealed/nested/anonymous/local/inner, generics (bounded/wildcards/
  inference/recursive bounds), lambdas + method refs, streams, Optional, pattern
  matching, text blocks/var/varargs/arrays, exceptions/try-with-resources/multi-catch,
  annotations + reflection, `java.util` collections, `java.util.concurrent`,
  `java.time`, `java.util.regex`, `java.nio`, `String`/`Math`/`BigInteger`/`BigDecimal`.
- **`update-alternatives` version switch** — for each JDK,
  `update-alternatives --set java /opt/jdkN/bin/java` (modeled as the
  `/usr/bin/java → /etc/alternatives/java → /opt/jdkN/bin/java` symlink chain) and
  assert `java -version` reports the right major.
- **sdkman-style candidate switch** — offline `sdk use java N-open` over a
  pre-seeded `~/.sdkman/candidates/java` layout.
- **Backward compatibility** (`BackCompat.java`) — compiled **once** with
  `javac --release 17` (class file version 61), then run **unchanged** on every
  staged JDK; the output must be byte-identical across versions (the JVM
  back-compat guarantee).
- **Real-world forward compatibility** (`backcompat/`, `BackCompatReal`) — a
  **299-test JUnit suite** over real third-party libraries (Apache Commons IO /
  Math3 / Lang3 / Collections4, Log4j2, H2 + HSQLDB, Gson, BeanShell), compiled
  **`--release 8`** (class file version **52**) and run **unchanged** on every
  runnable JDK. Each JDK must print the **identical** `BACKCOMPAT_REAL_OK 299`
  token — a real cross-version proof that a Java-8 jar runs on JDK 17/21/23/25.
  The 12 dependency jars are fetched from Maven Central by sha256 (`prebuild.sh`
  `stage_backcompat`); the jar itself is recompiled in-prebuild with a host
  `javac --release 8` when present (reproducible), else the prebuilt jar is copied.

## Per-arch JDK set

The per-arch JDK cells are byte-for-byte the **PROVEN `openjdk-multi`** delivery
(`download/jdk-multi`, `hw4os-s5d1t2/java/jdk-multi`). All are **native musl** (no
gcompat); `javac` works on every staged cell.

| arch | JDK17 | JDK21 | JDK23 | JDK25 | switch |
| :--: | :--: | :--: | :--: | :--: | :--: |
| x86_64 | apk (openjdk17) | BellSoft musl | BellSoft musl | BellSoft musl | 4/4 (via #1366+#1367) |
| aarch64 | apk (openjdk17) | BellSoft musl | BellSoft musl | BellSoft musl | 4/4 |
| riscv64 | native-musl cross | Alpine-musl | BellSoft glibc + Debian rt | native-musl src-build | 4/4 |
| loongarch64 | apk (openjdk17-loong) | Alpine-musl | native-musl src-build | Alpine-musl | 4/4 |

All four arches now run the **full `17/21/23/25`** set with no documented runtime
SKIPs (verified against the real `run-java.sh` `RUNNABLE JDKs:` / `SKIPPED JDKs:`
gate output, not assumed):
- **riscv64 runs the full `17/21/23/25`.** JDK25 **no longer SKIPs** — the prebuilt
  riscv64 JDK25 server VMs emitted the reserved compressed instruction `C.LUI x5,0`
  (`0x6281`) → `IllegalInstruction` on RV64GC; `prebuild.sh` now stages a **native
  riscv64-musl JDK25 built from source** (`openjdk25-riscv64-musl-srcbuild.tar.gz`,
  via `setup-rv-jdk25.sh`), cross-compiled from `openjdk/jdk25u` tag `jdk-25.0.4+5`
  with `rv-jdk25-musl-port.patch` which guards the `lui→c_lui` peephole with
  `(imm & 0xfff)==0` so it never emits `C.LUI rd,0` (see the per-JDK gate below).
  JDK23 still **runs** via BellSoft generic-glibc JDK23 plus a staged real
  Debian-trixie glibc runtime (unchanged), and the probe passes.
- **loongarch64 runs the full `17/21/23/25`.** Upstream ships no musl JDK23 for
  loongarch64 (Alpine has 17/21/25 only; the Loongson glibc build is old-abi/abi1.0 and
  does not run under the upstream abi), so `prebuild.sh` stages a **native loongarch64-musl
  JDK23 cross-compiled from source** (loongson/jdk tag `jdk-23+25-ls-0`, built against the
  `loongarch64-linux-musl` toolchain — see `loong-jdk23-musl-port.patch`). Two musl-loader
  compat symlinks (`stage_loong_musl_compat`) bridge the cross-toolchain loader naming to
  the Alpine rootfs, and the probe passes.

All four arches therefore run the **full 4 JDKs** with a **4/4** version switch
(**16/16** across the matrix). On every arch, `javac`, `java`, the
language/grammar/CLI carpets, and BackCompat run on every one of the 4 staged JDKs.
**x86_64** is runnable once its two companion kernel fixes (#1366 + #1367) merge —
see the **x86_64 status** note below.

### riscv64 — the RV64GC baseline + why no `-XX` flags are needed

riscv64 now runs the **full `17/21/23/25`** with **zero documented SKIPs**. JDK25
used to SKIP because the prebuilt riscv64 server VMs emitted the reserved compressed
instruction `C.LUI x5,0` (`0x6281`) from JIT-generated code → `IllegalInstruction`
on RV64GC. That is now fixed at the source: `prebuild.sh` stages a native
riscv64-musl JDK25 (`openjdk25-riscv64-musl-srcbuild.tar.gz`) built from
`openjdk/jdk25u` tag `jdk-25.0.4+5` with `rv-jdk25-musl-port.patch`, which guards the
`lui→c_lui` peephole with `(imm & 0xfff)==0` so the JIT never emits `C.LUI rd,0`.

The technical background on the StarryOS rv64 baseline is still worth recording, and
it explains why **no JVM `-XX` ISA flags are needed**. The HotSpot RISC-V port probes
ISA extensions via `riscv_hwprobe` (syscall 258) and `getauxval(AT_HWCAP)`; StarryOS
reports the **RV64GC baseline** (`IMA + FD + C`, no `Zba/Zbb/Zbs`, no vector `V`).
QEMU `-cpu rv64` already enables `Zba/Zbb/Zbc/Zbs` (bitmanip is pure userspace and
needs no kernel support), but the **vector `V` extension is OFF and CANNOT be
enabled by widening `-cpu`**: the generic StarryOS riscv64 kernel does not manage
vector state (`sstatus.VS` is left `Off` outside the `xuantie-c9xx` board build, and
there is no vector-register context save/restore), so any guest `vsetvli` traps as
`IllegalInstruction` even with `v=true`. Since the JIT targets that baseline anyway
(it never emits vector stubs), the gate adds **no** rv-specific flags —
`run-java.sh`'s `rvflags()` is intentionally a **no-op** (`rvflags() { echo ""; }`,
see `programs/run-java.sh`).

The gate keeps its runtime liveness probe as a defensive net (`programs/run-java.sh`):
each JDK is liveness-probed (`java -version`) before its suites run; **JDK17 always
runs** and is the carpet base. With the JDK25 source-build fix in place, riscv64 now
has **zero documented SKIPs** — all four JDKs probe live and run. (The SKIP
machinery remains, per the partial-arch-deliver rule: a JDK that genuinely cannot
run would be printed as a `SKIP` and removed from the attempted set rather than
counted as a failure, but no arch currently triggers it.) On `aarch64`,
`loongarch64`, and `x86_64` (once #1366 + #1367 merge) no JDK is skipped either.

### x86_64 — status (runs 4/4, gated on #1366 + #1367)

On `x86_64` all four JDKs stage, `javac` works, and the **on-target JVM run now
completes 4/4** (`AGGREGATE PASS=13/13` + `TEST PASSED`). Getting there root-caused
and fixed two x86_64-only StarryOS kernel bugs (companion PRs, not yet merged); the
x86_64 cell is enabled — and `qemu-x86_64.toml` ships — but is **Blocked-by: #1366 +
#1367** until those merge:

1. **#1366 — FXSAVE x87 tag-word seeded wrong (the real "layer-2 hang").**
   `ExtendedState::default` seeded the FXSAVE *abridged* x87 tag word `ftw` to
   `0xFFFF` ("stack full") instead of `0x00` ("empty"). On the qemu64
   FXRSTOR-fallback path, every new thread's first x87 op overflowed the x87 stack →
   musl long-double `fmt_fp` overran into the `%fs:0` TLS self-pointer → a recursive
   `%fs:0 == 0` `SIGSEGV` storm. This — **not** a busy-spin interpreter hang — was
   the actual cause of what earlier drafts called the "layer-2 hang". Fix: seed
   `ftw = 0x00`.
2. **#1367 — integer-divide `#DE` delivered as `SIGTRAP` instead of `SIGFPE`.**
   x86 integer divide-by-zero (`#DE`) was delivered to userspace as `SIGTRAP` rather
   than `SIGFPE`/`FPE_INTDIV`, so HotSpot could not turn an `idiv`-by-zero into an
   `ArithmeticException` and `javac` aborted on larger compiles. Fix: deliver
   `SIGFPE` with `si_code = FPE_INTDIV`.

(The earlier `siginfo.si_addr == 0` POSIX fix, **rcore-os/tgoskits#1331**, is also
required and is already part of the kernel baseline; it eliminated the original
near-null fault loop. #1366 + #1367 are the remaining two.)

Because the StarryOS QEMU **app** workflow for `apps/starry/**` is **path-filtered
out of CI** (the same as every other `apps/starry` carpet, e.g. python-lang), the
x86_64 cell does not block this PR's CI. Once #1366 + #1367 merge, x86_64 runs the
full `17/21/23/25` exactly like the other three arches.

## Rootfs sizing — how the full JDKs fit

The apps/starry harness copies the **1 GiB** base Alpine rootfs to a per-app image,
runs `prebuild.sh` (handing it `STARRY_ROOTFS` = that image + `STARRY_OVERLAY_DIR`),
then injects the overlay into the image via `debugfs` **without resizing it**. Four
full JDKs (~1.5 GiB on-disk) do not fit in 1 GiB → `debugfs` silently truncates
large files (`libjvm.so`) → `dlopen` `ENOEXEC` (the `kotlin-lang` "Exec format
error", which that case dodged by dropping to a 156 MB JRE — not an option here
because `javac` needs the full JDK).

**Fix:** `prebuild.sh` grows `$STARRY_ROOTFS` to **6 GiB** in place
(`truncate -s 6G` + `e2fsck -f -y` + `resize2fs`) **before** the harness injects —
exactly as the proven `prep-jdk-multi-rootfs.sh` grows its image to 6 GiB. The
`qemu-<arch>.toml` drive points at this same per-app image
(`rootfs-<arch>-alpine.img`), so the grown image is what boots. The growth is
host-side disk only; the running QEMU only ever maps a `-Xmx512m` JVM.

## Layout

```text
apps/starry/java-lang/
  prebuild.sh                  # grow rootfs to 6G + stage full per-arch JDK(s) into overlay
  build-<target>.toml          # StarryOS build config (4 targets)
  qemu-<arch>.toml             # QEMU run config (4 arches: x86_64/aarch64/riscv64/loongarch64; x86_64 gated on #1366+#1367)
  programs/
    Jdk17Features.java         # per-version language/stdlib feature self-tests
    Jdk21Features.java
    Jdk23Features.java         # (runs on all 4 arches)
    Jdk25Features.java
    JavaGrammar.java           # full-JLS grammar carpet (JAVA_GRAMMAR_OK)
    JavaLangCarpet.java        # 316-chk language carpet (compiled on-target, JAVA_LANG_OK)
    BackCompat.java            # cross-version backward-compat (compile@17, run on all)
    java-cli-core.sh           # kernel-relevant javac/java CLI option carpet (JAVA_CLI_OK)
    java-toolchain-carpet.sh   # jshell + JDK dev toolchain (jar/javap/javadoc/jdeps/jlink/jmod/jpackage/...) carpet (JAVA_TOOLCHAIN_OK)
    run-java.sh                # on-target aggregate gate (staged to /usr/bin; shell_init_cmd runs it)
    backcompat/
      README.md                # the real-world Java-8 forward-compat suite + lib coords/sha256
      src/                     # BackCompatReal + 5 *BackCompatTest sources (compiled --release 8)
```

The aggregate gate logic lives in **`programs/run-java.sh`**, staged into the rootfs
at `/usr/bin/run-java.sh`; each `qemu-<arch>.toml` sets
`shell_init_cmd = "sh /usr/bin/run-java.sh"`. The gate is a **staged script, not an
inline `shell_init_cmd`**, on purpose: the StarryOS app harness echoes the
`shell_init_cmd` text back over the serial console, so an inline `echo "TEST PASSED"`
would land verbatim in the captured stream and be matched by
`success_regex = (?m)^TEST PASSED$` as a **false positive** (it would "pass" even
when the real gate prints `TEST FAILED`). Staged, the only echoed text is
`sh /usr/bin/run-java.sh`, so the regex only ever matches the gate's REAL stdout
(same pattern as `node-lang/run_node_carpet.sh`). The script detects the arch + JDK
set at run time, so one script serves all four arches.

Guest layout (== `openjdk-multi`): `/opt/jdk{17,21,23,25}` (update-alternatives
candidate roots), `/opt/jdk-current` symlink, `~/.sdkman/candidates/java/*`,
`/root/jdkm/*.java`. The **per-JDK** musl loader path
(`/etc/ld-musl-<arch>.path`) is set by `run-java.sh` (`setp`/`LDP=/etc/ld-musl-$ARCH.path`)
at run time, **one JDK at a time** — a single shared path listing all JDK lib dirs makes JDK 23/25 mis-resolve
their runtime libs to the first entry (jdk17) and fails the source launcher
(root-caused + validated host+starry).

## Run

```bash
cargo xtask starry app qemu -t java-lang --arch x86_64
cargo xtask starry app qemu -t java-lang --arch aarch64
cargo xtask starry app qemu -t java-lang --arch riscv64
cargo xtask starry app qemu -t java-lang --arch loongarch64
```

The staged gate (`run-java.sh`) forces `-Xint` (StarryOS JIT is unstable, #206) and
sets both `-Xms` and `-Xmx` (a bare ergonomic-heap JVM hits "Too small maximum heap"
on starry). The aggregate gate prints `TEST PASSED`
(`success_regex = (?m)^TEST PASSED\s*$`) only when `PASS == TOTAL` over the genuinely
attempted suites; any `SUITE FAIL`, a `TEST FAILED`, or a JVM `panic` makes the run
fail fast. All four arches currently run the full `17/21/23/25` with **zero
documented SKIPs**; the SKIP machinery (excluded from `TOTAL`, never a failure)
remains as a defensive net per the partial-arch-deliver rule but is not triggered.

> **x86_64 is a runnable target in this PR** — it ships a `qemu-x86_64.toml` and runs
> the full `17/21/23/25` (4/4). It is **Blocked-by: #1366 + #1367** (the two
> x86_64-only StarryOS kernel fixes in the **x86_64 status** note) until those merge.
> Note the local app-qemu path (`-kernel`, no PVH note) cannot boot x86 StarryOS via
> `qemu-system-x86_64 -kernel` directly; x86_64 boots through the harness's
> UEFI/OVMF path. apps/starry QEMU jobs are path-filtered out of CI for every arch.

## Host golden evidence

The full `javac`/`java` launcher surface (every `--help`/`--help-extra`/`-X`
option, the 68-check host golden referenced by `java-cli-core.sh` /
`java-toolchain-carpet.sh`) is validated host-side and delivered with the carpet
sources (it is not part of the on-target `programs/` tree, since the full surface is
not meaningfully kernel-dependent). The on-target run ships and uses the
kernel-relevant `java-cli-core.sh` subset (`JAVA_CLI_OK`, tractable under QEMU TCG)
plus the on-target compile + run of the full `JavaLangCarpet.java`
(`JAVA_LANG_OK`).
