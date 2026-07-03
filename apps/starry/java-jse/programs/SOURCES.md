# java-jse — asset provenance (source-only repo)

This app commits **only source + manifests**. No compiled `.jar` and no native `.so` are
checked in. `prebuild.sh` fetches every third-party dependency from Maven Central by sha256,
compiles the carpet classes in-prebuild with `javac --release 17`, and stages the result into
the per-app rootfs. Every arch — including riscv64 — is provisioned by a download, so a clean
checkout runs end-to-end without any pre-seeded private cache. This file records the provenance
of every fetched / built asset.

## Third-party dependency jars (Maven Central, fetched by sha256)

Base URL: `https://repo1.maven.org/maven2/` + `<maven-path>`. Each sha256 was verified against
Maven Central's published `.sha1` for the same artifact. These are RUNTIME classpath deps
(staged into `/root/jse/libs/`); `lombok` is compile-only (see below).

| artifact | maven-path | sha256 |
|:--|:--|:--|
| jackson-databind 2.17.2 | com/fasterxml/jackson/core/jackson-databind/2.17.2/jackson-databind-2.17.2.jar | c04993f33c0f845342653784f14f38373d005280e6359db5f808701cfae73c0c |
| jackson-core 2.17.2 | com/fasterxml/jackson/core/jackson-core/2.17.2/jackson-core-2.17.2.jar | 721a189241dab0525d9e858e5cb604d3ecc0ede081e2de77d6f34fa5779a5b46 |
| jackson-annotations 2.17.2 | com/fasterxml/jackson/core/jackson-annotations/2.17.2/jackson-annotations-2.17.2.jar | 873a606e23507969f9bbbea939d5e19274a88775ea5a169ba7e2d795aa5156e1 |
| guava 33.2.1-jre | com/google/guava/guava/33.2.1-jre/guava-33.2.1-jre.jar | 452b2d9787b7d366fa8cf5ed9a1c40404542d05effa7a598da03bbbbb76d9f31 |
| failureaccess 1.0.2 | com/google/guava/failureaccess/1.0.2/failureaccess-1.0.2.jar | 8a8f81cf9b359e3f6dfa691a1e776985c061ef2f223c9b2c80753e1b458e8064 |
| listenablefuture 9999.0-empty-to-avoid-conflict-with-guava | com/google/guava/listenablefuture/9999.0-empty-to-avoid-conflict-with-guava/listenablefuture-9999.0-empty-to-avoid-conflict-with-guava.jar | b372a037d4230aa57fbeffdef30fd6123f9c0c2db85d0aced00c91b974f33f99 |
| jsr305 3.0.2 | com/google/code/findbugs/jsr305/3.0.2/jsr305-3.0.2.jar | 766ad2a0783f2687962c8ad74ceecc38a28b9f72a2d085ee438b7813e928d0c7 |
| error_prone_annotations 2.26.1 | com/google/errorprone/error_prone_annotations/2.26.1/error_prone_annotations-2.26.1.jar | de25f2d9a2156529bd765f51d8efdfc0dfa7301e04efb9cc75b7f10cf5d0e0fb |
| j2objc-annotations 3.0.0 | com/google/j2objc/j2objc-annotations/3.0.0/j2objc-annotations-3.0.0.jar | 88241573467ddca44ffd4d74aa04c2bbfd11bf7c17e0c342c94c9de7a70a7c64 |
| commons-lang3 3.14.0 | org/apache/commons/commons-lang3/3.14.0/commons-lang3-3.14.0.jar | 7b96bf3ee68949abb5bc465559ac270e0551596fa34523fddf890ec418dde13c |
| h2 2.2.224 | com/h2database/h2/2.2.224/h2-2.2.224.jar | b9d8f19358ada82a4f6eb5b174c6cfe320a375b5a9cb5a4fe456d623e6e55497 |
| slf4j-api 2.0.13 | org/slf4j/slf4j-api/2.0.13/slf4j-api-2.0.13.jar | e7c2a48e8515ba1f49fa637d57b4e2f590b3f5bd97407ac699c3aa5efb1204a9 |
| slf4j-simple 2.0.13 | org/slf4j/slf4j-simple/2.0.13/slf4j-simple-2.0.13.jar | 3153fe1d689cffb94f1530b58470c306685ba68844de8857116e3b6ebb81d9f7 |
| logback-classic 1.5.6 | ch/qos/logback/logback-classic/1.5.6/logback-classic-1.5.6.jar | 6115c6cac5ed1d9db810d14f2f7f4dd6a9f21f0acbba8016e4daaca2ba0f5eb8 |
| logback-core 1.5.6 | ch/qos/logback/logback-core/1.5.6/logback-core-1.5.6.jar | 898c7d120199f37e1acc8118d97ab15a4d02b0e72e27ba9f05843cb374e160c6 |
| sqlite-jdbc 3.46.1.3 | org/xerial/sqlite-jdbc/3.46.1.3/sqlite-jdbc-3.46.1.3.jar | 4a4832720a65eaf7f4d6fd7ede52087b994dc5633c076f9e994dc0c8b4b0b4fa |
| lombok 1.18.34 (compile-only) | org/projectlombok/lombok/1.18.34/lombok-1.18.34.jar | c27d6b2aff56241d1b07fcbcc6b183709e6b432c80f7374eeb1d823e86d4b81a |

These versions are exactly the coordinates that the previously-committed fat "demo jars"
bundled (read from their `META-INF/maven/**/pom.xml` + `pom.properties`): realdep = jackson +
guava + commons-lang3; jdbc = h2 + slf4j + logback; sqlite = sqlite-jdbc + slf4j + slf4j-simple.
`lombok` compiles `LombokCarpet` (its annotations are `RetentionPolicy.SOURCE`, so it is on the
compile classpath / `--processor-path` only and is not needed — nor staged — at run time).

## Source → carpet-class mapping (compiled in-prebuild into `carpets.jar`)

`javac --release 17 --processor-path lombok -cp <all deps> programs/{lib-carpets,jse-suite}/*.java`

| source | class | compile deps | runtime deps (run-jse.sh classpath) |
|:--|:--|:--|:--|
| programs/lib-carpets/JacksonCarpet.java | org.starry.dod.JacksonCarpet | jackson-databind/core/annotations | same |
| programs/lib-carpets/GuavaCarpet.java | org.starry.dod.GuavaCarpet | guava (+ failureaccess/jsr305/errorprone/j2objc) | same |
| programs/lib-carpets/Lang3Carpet.java | org.starry.dod.Lang3Carpet | commons-lang3 | same |
| programs/lib-carpets/H2Carpet.java | org.starry.dod.H2Carpet | h2 | h2 + slf4j-api + logback-classic + logback-core |
| programs/lib-carpets/LogCarpet.java | org.starry.dod.LogCarpet | slf4j-api + logback-classic/core | same |
| programs/lib-carpets/SqliteJdbcCarpet.java | org.starry.dod.SqliteJdbcCarpet | (JDK only; loads driver via Class.forName) | sqlite-jdbc + slf4j-api + slf4j-simple |
| programs/jse-suite/LombokCarpet.java | org.starry.dod.LombokCarpet | lombok | (JDK only) |
| programs/jse-suite/{AlgoTest,ConcurrencyTest,ConcurrencyDeep,CryptoTest,ExtraTest,FileTest,JvmTest,LangUtilTest,NetTest,NioChannelTest,ProcessTest,StdlibTest,SyntaxTest,TimeTest,XmlTest}.java | default-package test classes | JDK only | JDK only |

## OpenJDK 17 (per-arch; StarryOS runs both musl and glibc)

StarryOS is libc-agnostic, so any prebuilt JDK17 of the matching major version works regardless
of the libc it was built against. Every arch is provisioned by a download on a clean checkout.

- **x86_64 / aarch64**: Alpine v3.22/community `openjdk17-*` apks (musl; rolling patch level —
  sha unpinned, cache copy authoritative, `JDK17_X86AA_VER` default `17.0.19_p10-r0`).
- **loongarch64**: Alpine edge/community `openjdk17-loongarch-*` apks `17.0.17_p10-r0` (musl;
  sha256 pinned in `prebuild.sh`).
- **riscv64**: a **downloadable prebuilt glibc JDK17** — Adoptium Temurin `17.0.19+10` — because
  **Alpine ships no riscv64 openjdk17** (only openjdk21/25 for riscv64). Bridged by a staged real
  Debian glibc runtime closure (the JDK's own `ld-linux-riscv64-lp64d.so.1` interpreter loads it).

  | asset | URL | sha256 |
  |:--|:--|:--|
  | JDK17 tarball | https://github.com/adoptium/temurin17-binaries/releases/download/jdk-17.0.19%2B10/OpenJDK17U-jdk_riscv64_linux_hotspot_17.0.19_10.tar.gz | 191cdd904aef8b8a7a91c98d649c7e3dc75b7341f112061231c2094c418fd630 |
  | glibc runtime | http://deb.debian.org/debian/pool/main/g/glibc/libc6_2.41-12+deb13u3_riscv64.deb | fee42ebb2a148cc0dbc46ba938d8d69495b6dd5250cecafed9d585c567550b7a |

  The JDK statically bundles zlib/libstdc++/libgcc, so its only external closure is `libc6`
  (verified via `readelf -d`: `libc.so.6 libm.so.6 libpthread.so.0 libdl.so.2 librt.so.1`), which
  the Debian `libc6` deb supplies (into `/usr/lib/riscv64-linux-gnu` + the loader at
  `/lib/ld-linux-riscv64-lp64d.so.1`). This is byte-for-byte the same glibc-runtime bridge the
  merged `java-lang` app stages for its riscv64 JDK23 cell (`stage_real_glibc_rv`). BellSoft
  Liberica generic-glibc riscv64 and the Debian apt `openjdk-17-jdk-headless:riscv64` build are
  equivalent downloadable sources of the same JDK17 (override via `JDK17_RISCV_URL` /
  `JDK17_RISCV_SHA`).

## sqlite-jdbc native JNI (`libsqlitejdbc.so`), per arch

- **x86_64 / aarch64**: the fetched `sqlite-jdbc-3.46.1.3.jar` BUNDLES a musl JNI at
  `org/sqlite/native/Linux-Musl/{x86_64,aarch64}/libsqlitejdbc.so`; the driver self-extracts and
  `dlopen`s it at run time. Nothing is staged, nothing is committed.
- **riscv64**: the rv64 JDK17 is the prebuilt **glibc** build, so the matching JNI is the jar's
  **own bundled glibc riscv64 native** at `org/sqlite/native/Linux/riscv64/libsqlitejdbc.so`
  (its only external NEEDED are `libm.so.6` + `libc.so.6`, both in the staged Debian glibc
  closure). `prebuild.sh` extracts it from the already-fetched, sha256-pinned jar — no extra
  download and no cross-build — and stages it at `/root/jse/native/libsqlitejdbc.so`;
  `run-jse.sh` points `org.sqlite.lib.path` at it.
- **loongarch64**: the upstream jar ships **no** loongarch64 native at all (neither
  `Linux/loongarch64/` nor `Linux-Musl/loongarch64/`), and no vendor prebuilds one, so the loong
  JDK17 (Alpine-musl) needs a musl loongarch64 JNI that is **cross-compiled in-prebuild from
  official source** — `prebuild.sh`'s `build_loong_sqlite_jni` reproduces xerial/sqlite-jdbc's own
  native build. It is **reproducible** (the two source inputs are sha256-pinned) and **not
  committed**; it needs the `loongarch64-linux-musl-gcc` cross-toolchain (StarryOS `.starry-env.sh`
  PATH). Steps, exactly as xerial's `Makefile`:

  | source input | URL | sha256 |
  |:--|:--|:--|
  | sqlite-jdbc source (tag 3.46.1.3) | https://github.com/xerial/sqlite-jdbc/archive/refs/tags/3.46.1.3.tar.gz | 5d662eb23a0db84ef597ef1800811a6dc42727e0d5fc43b752efd3224dc2695c |
  | SQLite amalgamation 3.46.1 | https://www.sqlite.org/2024/sqlite-amalgamation-3460100.zip | 77823cb110929c2bcb0f5d48e4833b5c59a8a6e40cdea3936b99e199dbbe5784 |

  1. generate `NativeDB.h` with the host `javac -h` from `src/main/java/org/sqlite/core/NativeDB.java`
     (the fetched `sqlite-jdbc` + `slf4j-api-2.0.13` jars on the classpath);
  2. patch `sqlite3.c` with xerial's two `perl` edits (register the extension functions on
     `openDatabase`, add the `JDBC_EXTENSIONS` compile-option) and append `src/main/ext/*.c`;
  3. `loongarch64-linux-musl-gcc -Os -fPIC -fvisibility=hidden` compile `sqlite3.o` (with xerial's
     exact `-DSQLITE_ENABLE_FTS3/FTS5/RTREE/STAT4/DBSTAT_VTAB/COLUMN_METADATA/MATH_FUNCTIONS/…`
     flag set) + `NativeDB.o`, link `-shared -static-libgcc -pthread -lm`, strip.

  The result is a LoongArch ELF64 musl shared object (`NEEDED: libc.so`, exporting the 61
  `Java_org_sqlite_core_NativeDB_*` symbols, embedding SQLite 3.46.1) staged at
  `/root/jse/native/libsqlitejdbc.so`; `run-jse.sh` points `org.sqlite.lib.path` at it. Its own
  sha256 is **not** pinned (it varies with the cross-gcc version) — reproducibility is anchored on
  the two sha256-pinned source inputs + the fixed recipe. A developer who has already built it can
  seed the cache (`$JAVA_DL_ROOT/sqlitejdbc-native/loongarch64/libsqlitejdbc.so`) to skip the
  ~1-min compile. **Only** if the loong musl cross-toolchain is genuinely unavailable does the
  sqlite carpet degrade to a documented SKIP on loongarch64 (partial-arch-deliver, decided at run
  time by `run-jse.sh`) — never a hard exit and never a silent fallback.
