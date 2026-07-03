# java-web — JEE framework carpet

Industrial-grade, on-target test of a set of real JEE/JVM frameworks, run by **OpenJDK 17**
on StarryOS across all four architectures (x86_64 / aarch64 / riscv64 / loongarch64).

Each module is a self-contained carpet that exercises one framework's public API surface
(dozens-to-hundreds of exact-value assertions; HTTP servers are driven over real IPv4 loopback
with `HttpURLConnection`, ORMs run against an in-memory database). A module prints an anchored
`*_DONE` marker only when its internal fail count is zero; `run-jweb.sh` runs every module
and emits `TEST PASSED` only when every attempted module passes.

## Source-only, reproducible build

This app commits **only source + manifests** — no compiled framework `.jar` and no native
`.so`. `prebuild.sh` (the merged java-lang / java-jse model):

1. fetches every third-party dependency from **Maven Central by sha256** into a cache
   (`JAVA_DL_ROOT`), re-used network-free on later runs;
2. compiles the six carpet classes in-prebuild with the **host** `javac --release 17` from
   `programs/carpets/*.java` (arch-independent bytecode, cached across the four arches);
3. stages a full per-arch JDK17 + `carpets.jar` + the dependency jars into the per-app rootfs
   (grown grow-only to 2.5G); each module runs with `-Xint -Xmx512m`.

A **clean checkout can run every architecture end-to-end** — each arch's JDK17 (and every
dependency) is provisioned by a download; nothing is expected to pre-exist in a private cache.
`programs/SOURCES.md` records the coordinate + sha256 of every fetched dependency and the
provenance of the JDK17 and the sqlite JNI. A developer who already has the assets points
`JAVA_DL_ROOT` at their cache and every fetch short-circuits.

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
  Debian glibc runtime" mechanism the merged `java-lang` app uses for its riscv64 JDK23 cell, and
  is identical to the merged `java-jse` case. (BellSoft Liberica generic-glibc and the Debian apt
  `openjdk-17` riscv64 build are equivalent sources of the same JDK.)

## Run

```
cargo xtask starry app qemu -t java-web --arch x86_64
cargo xtask starry app qemu -t java-web --arch aarch64
cargo xtask starry app qemu -t java-web --arch riscv64
cargo xtask starry app qemu -t java-web --arch loongarch64
```

## Coverage

| module | framework | dimension | marker |
|:--|:--|:--|:--|
| jetty | Eclipse Jetty 11.0.21 | embedded HTTP server (handlers / routing / methods / status-body-header assertions over loopback) | `JETTY_DONE` |
| netty | Netty 4.1.112 | ByteBuf, EmbeddedChannel codec/handler unit tests, real loopback TCP echo + HTTP-codec server | `NETTY_DONE` |
| mybatis | MyBatis 3.5.16 | SqlSessionFactory / mappers / annotations / dynamic SQL / batch / transactions over an in-memory DB | `MYBATIS_DONE` |
| hibernate | Hibernate ORM 6.4.4 / JPA 3.1 | SessionFactory / entities / CRUD / HQL-JPQL / Criteria / relationships / paging over an in-memory DB | `HIBERNATE_DONE` |
| r2dbc | R2DBC 1.0 | reactive ConnectionFactory / Statement / Result, deterministic subscription, transactions | `R2DBC_DONE` |
| war | Jakarta Servlet 5.0.2 | a real `.war` (servlet + `web.xml`) compiled + deployed into an embedded Jetty container, hit over loopback HTTP | `WAR_DONE` |

The carpet sources live in `programs/carpets/`; the framework libraries are fetched from Maven
Central by `prebuild.sh` (see `programs/SOURCES.md`).

## sqlite-jdbc native JNI, per arch (MyBatis + Hibernate)

MyBatis and Hibernate run over sqlite-jdbc 3.46.1.3:

- **x86_64 / aarch64** — the jar bundles a musl JNI (`Linux-Musl/…`); the driver self-extracts it.
- **riscv64** — the rv64 JDK17 is the prebuilt **glibc** build, so the matching JNI is the jar's
  own bundled **glibc riscv64 native** (`org/sqlite/native/Linux/riscv64/libsqlitejdbc.so`).
  `prebuild.sh` extracts it from the sha256-pinned jar (no extra download, no cross-build), stages
  it, and points `org.sqlite.lib.path` at it.
- **loongarch64** — the upstream jar ships **no** loongarch64 native at all (neither glibc nor
  musl) and no vendor prebuilds one, so `prebuild.sh` **cross-compiles the musl loongarch64 JNI
  in-prebuild from xerial/sqlite-jdbc's official source** (reproducing xerial's own native build:
  `NativeDB.c` + the official SQLite amalgamation, `loongarch64-linux-musl-gcc`). It is
  reproducible (both source inputs are sha256-pinned; the small C lib compiles in ~1 min) and
  needs the loong musl cross-toolchain; see `programs/SOURCES.md`. **Only** if that toolchain is
  genuinely unavailable do the **MyBatis and Hibernate carpets degrade to a documented SKIP** on
  loongarch64 (partial-arch-deliver rule) — printed loudly with their reason and never counted as
  a pass or a failure. The jetty / netty / r2dbc / war carpets do not use sqlite and run normally.
