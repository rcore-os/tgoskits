# java-jse — J2SE library + JSE standard-library carpet

Industrial-grade, on-target test of a set of real J2SE third-party libraries and the JSE
standard library, run by **OpenJDK 17** on StarryOS across all four architectures
(x86_64 / aarch64 / riscv64 / loongarch64).

Each module is a self-contained carpet that exercises the full public API surface of one
library / JDK package — hundreds of exact-value assertions grounded in the official API
docs — and prints an anchored `*_DONE` marker only when its internal fail count is zero.
`run-jse.sh` runs every module and emits `TEST PASSED` only when every attempted module passes.
The suite is 22 modules / ~5650 assertions in total.

## Source-only repo (no committed binaries)

This app commits **only source + manifests** — the carpet `.java` under `programs/`, the build
scripts, and the pinned dependency coordinates (Maven `groupId:artifactId:version` + sha256).
No compiled `.jar` and no native `.so` is checked in. `prebuild.sh`:

1. fetches every third-party dependency jar from Maven Central **by sha256** into a cache
   (`JAVA_DL_ROOT`), re-used network-free on later runs;
2. **compiles the carpet classes in-prebuild** with `javac --release 17` from the committed
   sources (deps on the classpath, lombok on the annotation `--processor-path`) into
   `carpets.jar`;
3. stages `carpets.jar` + the dependency jars into the per-app rootfs at `/root/jse{,/libs}`,
   alongside a full per-arch JDK17.

A **clean checkout can run every architecture end-to-end** — each arch's JDK17 (and every
dependency) is provisioned by a download; nothing is expected to pre-exist in a private cache.
See `programs/SOURCES.md` for the exact dependency list (coord + sha256 + URL), the JDK17
provenance, and the native-JNI provenance. This mirrors the merged `java-lang` app's
reproducible `ensure_asset` + in-prebuild `javac` model.

## JDK17 per arch (StarryOS runs both musl and glibc)

StarryOS is libc-agnostic, so any prebuilt JDK17 of the matching major version works regardless
of the libc it was built against:

- **x86_64 / aarch64** — Alpine v3.22/community `openjdk17-*` apks (musl). Run directly by the
  default command below.
- **loongarch64** — Alpine edge/community `openjdk17-loongarch-*` apks (musl). Run directly.
- **riscv64** — a **downloadable prebuilt glibc JDK17** (Adoptium Temurin `17.0.19+10`), because
  **Alpine ships no riscv64 openjdk17** (only openjdk21/25 for riscv64). `prebuild.sh` stages a
  small **real Debian glibc runtime closure** so the JDK's own `ld-linux-riscv64-lp64d.so.1`
  interpreter resolves its `libc.so.6` references; the JDK statically bundles zlib/libstdc++/libgcc,
  so `libc6` is its entire external closure. This is the same "download a glibc JDK + stage a real
  Debian glibc runtime" mechanism the merged `java-lang` app uses for its riscv64 JDK23 cell.
  (BellSoft Liberica generic-glibc and the Debian apt `openjdk-17` riscv64 build are equivalent
  sources of the same JDK.)

## Run

```
cargo xtask starry app qemu -t java-jse --arch x86_64
cargo xtask starry app qemu -t java-jse --arch aarch64
cargo xtask starry app qemu -t java-jse --arch riscv64
cargo xtask starry app qemu -t java-jse --arch loongarch64
```

`prebuild.sh` grows the per-app rootfs to 2.5G so JDK17 + the jars fit; each module runs with
`-Xint -Xmx384m`. A developer who already has the JDK apks/tarball + dependency jars locally can
point `JAVA_DL_ROOT` at that cache to short-circuit the downloads. The in-prebuild compile needs
a host `javac` (JDK17+); if absent, `prebuild.sh` installs one via the host package manager.

## Coverage

J2SE third-party libraries (compiled from `programs/lib-carpets/`, dependency jars fetched from
Maven Central and staged under `/root/jse/libs/`):

| module | library | marker |
|:--|:--|:--|
| jackson | jackson-databind (streaming / databind / tree / annotations / polymorphic / features) | `JACKSON_DONE` |
| guava | Guava (immutable collections, Multimap/BiMap/Table/Multiset/RangeSet, hashing, cache, …) | `GUAVA_DONE` |
| lang3 | commons-lang3 (StringUtils/ArrayUtils/NumberUtils/builders/tuple/reflection/…) | `LANG3_DONE` |
| h2 | H2 JDBC (DDL/DML/DQL/joins/window/transactions/types/constraints) + `org.h2.tools.*` CLI | `H2_DONE` |
| log | slf4j + logback (levels, parameterized, MDC, programmatic appenders, pattern, filtering) | `LOG_DONE` |
| sqlite | xerial sqlite-jdbc (full JDBC + PRAGMA / type affinity / FK / triggers / CTE / UPSERT / json1) | `SQLITEJDBC_DONE` |
| lombok | lombok annotations (@Data/@Builder/@Value/@With/@NonNull/@SneakyThrows/@Cleanup/@Slf4j/…) | `LOMBOK_DONE` |

JSE standard-library carpets (`programs/jse-suite/*.java`, compiled into `carpets.jar`):
Algo, Concurrency, ConcurrencyDeep, Crypto, Extra, File, Jvm, LangUtil, Net, NioChannel,
Process, Stdlib, Syntax, Time, Xml — each covering the matching `java.*` package with its
own `*_DONE` marker.

## sqlite-jdbc native JNI, per arch

- **x86_64 / aarch64** — the fetched `sqlite-jdbc` jar bundles a musl JNI (`Linux-Musl/…`); the
  driver self-extracts and `dlopen`s it at run time. Nothing is staged.
- **riscv64** — the rv64 JDK17 is the prebuilt **glibc** build, so the matching JNI is the jar's
  **own bundled glibc riscv64 native** (`org/sqlite/native/Linux/riscv64/libsqlitejdbc.so`).
  `prebuild.sh` extracts it from the sha256-pinned jar (no extra download, no cross-build) and
  points `org.sqlite.lib.path` at it; it loads under the glibc JDK + the staged Debian glibc
  closure.
- **loongarch64** — the upstream jar ships **no** loongarch64 native at all (neither glibc nor
  musl) and no vendor prebuilds one, so `prebuild.sh` **cross-compiles the musl loongarch64 JNI
  in-prebuild from xerial/sqlite-jdbc's official source** (reproducing xerial's own native build:
  `NativeDB.c` + the official SQLite amalgamation, `loongarch64-linux-musl-gcc`). It is
  reproducible (both source inputs are sha256-pinned; the small C lib compiles in ~1 min) and
  needs the loong musl cross-toolchain; see `programs/SOURCES.md`. **Only** if that toolchain is
  genuinely unavailable does the **sqlite carpet degrade to a documented SKIP** on loongarch64
  (partial-arch-deliver rule) — printed loudly with its reason and never counted as a pass or a
  failure. The other 21 modules run normally.
