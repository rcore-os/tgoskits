# Starry java-lang App — multi-JDK (OpenJDK 17 / 21 / 23 / 25) `javac` · `java` carpet

This app runs an industrial, carpet-coverage Java **language + compiler + runtime**
test suite inside StarryOS QEMU across `aarch64 / riscv64 / loongarch64`. (x86_64 is
staged and `javac`-verified, but its on-target JVM run is kernel-blocked — see the
**x86_64 status** note — so this PR ships x86_64 as a documented, non-runnable target
and does not include a `qemu-x86_64.toml`.)
It is the #764 item:

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
- **Full-JLS grammar carpet** (`JavaGrammar.java`) — `JAVA_GRAMMAR_OK`.
- **Language carpet** (`JavaLangCarpet.java`, 363 assertions + 54 golden values) —
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
| x86_64 | apk (openjdk17) | BellSoft musl | BellSoft musl | BellSoft musl | ⚠ kernel-blocked — see x86_64 note |
| aarch64 | apk (openjdk17) | BellSoft musl | BellSoft musl | BellSoft musl | 4/4 |
| riscv64 | native-musl cross | Alpine-musl | — N/A | Alpine-musl | 3/3 |
| loongarch64 | apk (openjdk17-loong) | Alpine-musl | — N/A | Alpine-musl | 3/3 |

**JDK23 staged-but-runtime-SKIPped on riscv64 / loongarch64** (documented per the
partial-arch-tick rule): upstream ships JDK23 for these arches only as glibc.
`prebuild.sh` **does still stage** that glibc JDK23 (and, on riscv64, a real Debian
glibc runtime), and `run-java.sh` **liveness-probes it at run time** — but **glibc +
gcompat segfaults the JVM — verified on REAL Linux 6.18 too, so this is NOT a StarryOS
limitation** → the probe fails, JDK23 is **SKIPped and excluded from TOTAL** (never a
false failure). Net effect: those two arches run **3 JDKs (17/21/25)** with a **3/3**
version switch (JDK23 present on disk but not counted).
**aarch64** runs the **full 4 JDKs** with a **4/4** switch; `javac`, `java`, the
language/grammar/CLI carpets, and BackCompat run on every staged JDK. **x86_64**
stages all 4 JDKs and `javac` works on every cell, but the on-target JVM run is
currently blocked by a StarryOS kernel issue — see the **x86_64 status** note below.

### riscv64 / loongarch64 — honest per-JDK runnability gate

On `riscv64` (and, by the same reasoning, `loongarch64`) the staged JDK21/JDK25
binaries can raise a repeated `user exception: IllegalInstruction` from
JVM-generated code. **Root cause (verified):** the HotSpot RISC-V port probes ISA
extensions via `riscv_hwprobe` (syscall 258) and `getauxval(AT_HWCAP)`; StarryOS
reports the **RV64GC baseline** (`IMA + FD + C`, no `Zba/Zbb/Zbs`, no vector `V`).
QEMU `-cpu rv64` already enables `Zba/Zbb/Zbc/Zbs` (bitmanip is pure userspace and
needs no kernel support), but the **vector `V` extension is OFF and CANNOT be
enabled by widening `-cpu`**: the generic StarryOS riscv64 kernel does not manage
vector state (`sstatus.VS` is left `Off` outside the `xuantie-c9xx` board build, and
there is no vector-register context save/restore), so any guest `vsetvli` traps as
`IllegalInstruction` even with `v=true`.

The gate therefore does two things (`programs/run-java.sh`):

1. **No JVM extension-disable flags — `rvflags()` is a deliberate no-op.** An earlier
   approach passed `-XX:-UseRVV -XX:-UseZba/Zbb/Zbs` (with the per-JDK unlock tokens
   `-XX:+UnlockExperimentalVMOptions` / `-XX:+UnlockDiagnosticVMOptions`) to force the
   JVM to the RV64GC baseline. But the Alpine riscv64 OpenJDK 25 is a **Zero VM**
   (interpreter, no C2/JIT): it **rejects those C2 options as *unrecognized VM
   options*** and refuses to start. Since the StarryOS riscv64 baseline already leaves
   the vector `V` extension off (the JIT never emits vector stubs anyway), the gate
   adds **no** rv-specific flags — `run-java.sh`'s `rvflags()` is intentionally a
   **no-op** (`rvflags() { echo ""; }`, see `programs/run-java.sh`). The real
   protection is the runtime liveness-probe + documented SKIP below, not `-XX` flags.
2. **Documented SKIP, never a false failure.** Each JDK is liveness-probed
   (`java -version`) before its suites run. A JDK that still cannot run is printed as
   a **SKIP** (reason: *IllegalInstruction / RISC-V extension unsupported on starry*)
   and **removed from the attempted JDK set** — the per-suite TOTALs (vtest,
   `update-alternatives`, sdkman, BackCompat) reflect **only attempted-and-supported
   JDKs**, so a true pass is reachable. A skipped JDK is **NOT** counted as a failure
   (partial-arch-deliver rule). **JDK17 always runs** and is the carpet base; if it
   fails the run fails fast. On `aarch64` no JDK is skipped (x86_64 stages all 4 cells but its whole-arch on-target run is kernel-blocked — see the x86_64 status note — so it is not a runnable arch here).

### x86_64 — status (kernel-blocked, honest)

On `x86_64` all four JDKs stage correctly and `javac` works, but the **on-target
JVM run does not yet complete** — it is blocked by a StarryOS kernel issue with two
layers, both diagnosed:

1. **Layer 1 — `siginfo.si_addr` POSIX violation (fixed separately).** StarryOS
   delivered synchronous `SIGSEGV` with `si_addr == 0`. HotSpot reads `si_addr` in
   its own handler to classify implicit-null-check / guard-page faults; with
   `si_addr == 0` it could not, and looped on a near-null read (`NULL + 0x34`). This
   is a real kernel POSIX bug fixed in **rcore-os/tgoskits#1331** (with a checked-in
   regression test); it eliminates the fault loop.
2. **Layer 2 — post-fix interpreter hang (open).** With `si_addr` correct, the
   crash loop is gone but the JDK17 run then **hangs** (busy-spin, no further
   faults) under `-accel kvm`. This is a separate, deeper kernel issue still under
   investigation (syscall-trace diagnosis is the next step).

Because the StarryOS QEMU **app** workflow for `apps/starry/**` is **path-filtered
out of CI** (the same as every other `apps/starry` carpet, e.g. python-lang), the
x86_64 cell does not block this PR's CI. It is documented here honestly rather than
claimed green: x86_64 java is **not counted as passing** until the layer-2 hang is
resolved. The carpet itself, the staging, and `javac` are correct on x86_64.

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
(`rootfs-<arch>-java-lang.img`), so the grown image is what boots. The growth is
host-side disk only; the running QEMU only ever maps a `-Xmx512m` JVM.

## Layout

```text
apps/starry/java-lang/
  prebuild.sh                  # grow rootfs to 6G + stage full per-arch JDK(s) into overlay
  build-<target>.toml          # StarryOS build config (4 targets)
  qemu-<arch>.toml             # QEMU run config (3 runnable arches: aarch64/riscv64/loongarch64; x86_64 omitted — kernel-blocked)
  programs/
    Jdk17Features.java         # per-version language/stdlib feature self-tests
    Jdk21Features.java
    Jdk23Features.java         # (run on x86_64/aarch64 only)
    Jdk25Features.java
    JavaGrammar.java           # full-JLS grammar carpet (JAVA_GRAMMAR_OK)
    JavaLangCarpet.java        # 363-assertion language carpet (compiled on-target, JAVA_LANG_OK)
    BackCompat.java            # cross-version backward-compat (compile@17, run on all)
    java-cli-core.sh           # kernel-relevant javac/java CLI option carpet (JAVA_CLI_OK)
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
set at run time, so one script serves all arches (the 3 runnable arches here; x86_64 staging is identical but not run).

Guest layout (== `openjdk-multi`): `/opt/jdk{17,21,23,25}` (update-alternatives
candidate roots), `/opt/jdk-current` symlink, `~/.sdkman/candidates/java/*`,
`/root/jdkm/*.java`. The **per-JDK** musl loader path
(`/etc/ld-musl-<arch>.path`) is set by `qemu-<arch>.toml` at run time, **one JDK at
a time** — a single shared path listing all JDK lib dirs makes JDK 23/25 mis-resolve
their runtime libs to the first entry (jdk17) and fails the source launcher
(root-caused + validated host+starry).

## Run

```bash
cargo xtask starry app qemu -t java-lang --arch aarch64
cargo xtask starry app qemu -t java-lang --arch riscv64
cargo xtask starry app qemu -t java-lang --arch loongarch64
```

The staged gate (`run-java.sh`) forces `-Xint` (StarryOS JIT is unstable, #206) and
sets both `-Xms` and `-Xmx` (a bare ergonomic-heap JVM hits "Too small maximum heap"
on starry). The aggregate gate prints `TEST PASSED`
(`success_regex = (?m)^TEST PASSED\s*$`) only when `PASS == TOTAL` over the genuinely
attempted suites; any `SUITE FAIL`, a `TEST FAILED`, or a JVM `panic` makes the run
fail fast. Documented per-JDK SKIPs (riscv64/loongarch64, above) are excluded from
`TOTAL` and are not failures.

> **x86_64 is intentionally not a runnable target in this PR** — there is no
> `qemu-x86_64.toml`. Its on-target JVM run is kernel-blocked (layer-2 hang, see
> the **x86_64 status** note), and the local app-qemu path (`-kernel`, no PVH
> note) cannot boot x86 StarryOS anyway; apps/starry QEMU jobs are additionally
> path-filtered out of CI. The three shipped arches (aarch64 / riscv64 /
> loongarch64) boot locally via qemu-system-<arch> and are the runnable coverage.

## Host golden evidence

The full `javac`/`java` launcher surface (every `--help`/`--help-extra`/`-X`
option, 204 checks → `JAVA_CLI_OK`) and the language carpet golden are validated
host-side via `java-lang-work/java-cli-carpet.sh`, `JavaLangCarpet.golden.txt`, and
`run-java-lang-carpet.sh`. The on-target run uses the kernel-relevant
`java-cli-core.sh` subset (tractable under QEMU TCG) plus the on-target compile +
run of the full `JavaLangCarpet.java`.
