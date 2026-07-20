# java-web — asset provenance (source-only repo)

This app commits **only source + manifests**. No compiled framework `.jar` and no native
`.so` are checked in. `prebuild.sh` fetches every third-party dependency from Maven Central by
sha256, compiles the six carpet classes in-prebuild with `javac --release 17`, and stages the
result into the per-app rootfs. Every arch — including riscv64 — is provisioned by a download, so
a clean checkout runs end-to-end without any pre-seeded private cache. This file records the
provenance of every fetched / built asset.

## Third-party dependency jars (Maven Central, fetched by sha256)

Base URL: `https://repo1.maven.org/maven2/` + `<maven-path>`. Each sha256 was verified against
Maven Central's published `.sha1` for the same artifact. These are the exact coordinates the
previously-committed fat "demo jars" bundled: coordinates with a `pom.properties` were read
directly from `META-INF/maven/**/pom.properties`; the pom-less shaded deps (hibernate-core,
hibernate-community-dialects, hibernate-commons-annotations, jetty-jakarta-servlet-api, h2,
reactor-core, reactive-streams) were pinned by **byte-identical `.class` matching** against
Maven Central releases (see notes at the bottom). All are arch-independent JVM bytecode, so the
same set is staged for every architecture. `run-jweb.sh` assembles a curated per-module
classpath from this set.

### jetty + war module — embedded Eclipse Jetty 11.0.21 + Jakarta Servlet 5.0.2

| artifact | maven-path | sha256 |
|:--|:--|:--|
| jetty-server 11.0.21 | org/eclipse/jetty/jetty-server/11.0.21/jetty-server-11.0.21.jar | 466e5572a5e11253c01c92374cc2304861f1d75ef97c47237da2a59c35848aa5 |
| jetty-http 11.0.21 | org/eclipse/jetty/jetty-http/11.0.21/jetty-http-11.0.21.jar | 2a8d7acf56ec9586b58b8e779993181036190ec33ae5ca70acb4657fe364d233 |
| jetty-io 11.0.21 | org/eclipse/jetty/jetty-io/11.0.21/jetty-io-11.0.21.jar | 934d8a2a274724faca69ae0a51e71359ee25eae9bfcfbdda5db60a647ca1d609 |
| jetty-util 11.0.21 | org/eclipse/jetty/jetty-util/11.0.21/jetty-util-11.0.21.jar | ed26e0e1d0a3ac9bda98e7c211230f5d250f62117ffd46cc616d290beef18788 |
| jetty-jakarta-servlet-api 5.0.2 | org/eclipse/jetty/toolchain/jetty-jakarta-servlet-api/5.0.2/jetty-jakarta-servlet-api-5.0.2.jar | efb20997729f32bfa6c8a8319037c353f7ad460d5d49f336bf232998ea2358db |
| slf4j-api 2.0.9 | org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar | 0818930dc8d7debb403204611691da58e49d42c50b6ffcfdce02dadb7c3c2b6c |

### netty module — Netty 4.1.112.Final

| artifact | maven-path | sha256 |
|:--|:--|:--|
| netty-common 4.1.112.Final | io/netty/netty-common/4.1.112.Final/netty-common-4.1.112.Final.jar | b03967f32c65de5ed339b97729170e0289b22ffa5729e7f45f68bf6b431fb567 |
| netty-buffer 4.1.112.Final | io/netty/netty-buffer/4.1.112.Final/netty-buffer-4.1.112.Final.jar | bc182c48f5369d48cd8370d2ab0c5b8d99dd8ffa4a0f8ac701652d57bd380eff |
| netty-resolver 4.1.112.Final | io/netty/netty-resolver/4.1.112.Final/netty-resolver-4.1.112.Final.jar | 6b4ac9f3b67f562f0770d57c389279ff9c708eb401e1a3635f52297f0f897edc |
| netty-transport 4.1.112.Final | io/netty/netty-transport/4.1.112.Final/netty-transport-4.1.112.Final.jar | d38e31624d25ca790ee413d529c152170217ebedbcdcf61164fa6291f3a56c92 |
| netty-transport-native-unix-common 4.1.112.Final | io/netty/netty-transport-native-unix-common/4.1.112.Final/netty-transport-native-unix-common-4.1.112.Final.jar | e79ccea1b87a6348d4ebd3dfb37a2cccd9b7cb65c3375f6ccdac086c7b5ce487 |
| netty-codec 4.1.112.Final | io/netty/netty-codec/4.1.112.Final/netty-codec-4.1.112.Final.jar | 72db4f93629f7ea520d2998c08e2b1d69f9c6a4792b53da5e9a001d24c78b151 |
| netty-codec-http 4.1.112.Final | io/netty/netty-codec-http/4.1.112.Final/netty-codec-http-4.1.112.Final.jar | 21b502d1374d6992728d004e0c1c95544d46d971f55ea78dcb854ce1ac0c83bc |
| netty-handler 4.1.112.Final | io/netty/netty-handler/4.1.112.Final/netty-handler-4.1.112.Final.jar | ea4d6062a5fb10a6e2364d8bbdebc1cfa814f1fc9f910ef57e5caf02fb15c588 |

> The fat `netty-demo.jar` bundled `netty-all` 4.1.112.Final, but at 4.1.x `netty-all` on
> Maven Central is an **empty 4.5 KB aggregator** (no classes) — it only declares the split
> modules as dependencies. The carpet's `io.netty.*` imports resolve to the eight split modules
> above (`netty-handler` + `netty-transport-native-unix-common` are pulled in by
> `netty-codec-http`). `jctools` is shaded inside `netty-common`, so no separate jctools jar is
> needed.

### mybatis module — MyBatis 3.5.16

| artifact | maven-path | sha256 |
|:--|:--|:--|
| mybatis 3.5.16 | org/mybatis/mybatis/3.5.16/mybatis-3.5.16.jar | 1814d02fccd8dbeadf628cbac8962b1edaab9bfa67e8585c6a3663c48bd8953d |
| ognl 3.4.2 | ognl/ognl/3.4.2/ognl-3.4.2.jar | efb6bf5cb5bcad7a88932bd30a0861e5aac7382215fbd1f714ef59b739912852 |
| javassist 3.30.2-GA | org/javassist/javassist/3.30.2-GA/javassist-3.30.2-GA.jar | eba37290994b5e4868f3af98ff113f6244a6b099385d9ad46881307d3cb01aaf |

### shared by mybatis + hibernate — sqlite-jdbc + SLF4J

| artifact | maven-path | sha256 |
|:--|:--|:--|
| sqlite-jdbc 3.46.1.3 | org/xerial/sqlite-jdbc/3.46.1.3/sqlite-jdbc-3.46.1.3.jar | 4a4832720a65eaf7f4d6fd7ede52087b994dc5633c076f9e994dc0c8b4b0b4fa |
| slf4j-api 2.0.13 | org/slf4j/slf4j-api/2.0.13/slf4j-api-2.0.13.jar | e7c2a48e8515ba1f49fa637d57b4e2f590b3f5bd97407ac699c3aa5efb1204a9 |
| slf4j-simple 2.0.13 | org/slf4j/slf4j-simple/2.0.13/slf4j-simple-2.0.13.jar | 3153fe1d689cffb94f1530b58470c306685ba68844de8857116e3b6ebb81d9f7 |

### hibernate module — Hibernate ORM 6.4.4.Final + Jakarta Persistence 3.1

| artifact | maven-path | sha256 |
|:--|:--|:--|
| hibernate-core 6.4.4.Final | org/hibernate/orm/hibernate-core/6.4.4.Final/hibernate-core-6.4.4.Final.jar | a1324b7c80c800826c9a5d74b61b0de768141f967c2b082650cb7bf4675570a7 |
| hibernate-community-dialects 6.4.4.Final | org/hibernate/orm/hibernate-community-dialects/6.4.4.Final/hibernate-community-dialects-6.4.4.Final.jar | c448fc799cc079ffbb5d9fb8c32d1a6e62d6ab75c50ceaf67723466f4134f28a |
| hibernate-commons-annotations 6.0.6.Final | org/hibernate/common/hibernate-commons-annotations/6.0.6.Final/hibernate-commons-annotations-6.0.6.Final.jar | cd974e0a8481fafdbaf9b4a0f08bb5a6c969b0365482763eedf77e6fd7f493b7 |
| jakarta.persistence-api 3.1.0 | jakarta/persistence/jakarta.persistence-api/3.1.0/jakarta.persistence-api-3.1.0.jar | 475389446d35c6f46c565728b756dc508c284644ea2690644e0d8e7e339d42fd |
| jakarta.transaction-api 2.0.1 | jakarta/transaction/jakarta.transaction-api/2.0.1/jakarta.transaction-api-2.0.1.jar | 50c0a7c760c13ae6c042acf182b28f0047413db95b4636fb8879bcffab5ba875 |
| jboss-logging 3.5.0.Final | org/jboss/logging/jboss-logging/3.5.0.Final/jboss-logging-3.5.0.Final.jar | 7bb135b081952f6d32d83374619ae5201b05ca3bf862a28dd111016ce19b2c07 |
| jandex 3.1.2 | io/smallrye/jandex/3.1.2/jandex-3.1.2.jar | dee12fa1787d5523ed1a02d6c63b19e7aef6ac560f7c6d70595db01aa37e041e |
| classmate 1.5.1 | com/fasterxml/classmate/1.5.1/classmate-1.5.1.jar | aab4de3006808c09d25dd4ff4a3611cfb63c95463cfd99e73d2e1680d229a33b |
| byte-buddy 1.14.11 | net/bytebuddy/byte-buddy/1.14.11/byte-buddy-1.14.11.jar | 62ae28187ed2b062813da6a9d567bfee733c341582699b62dd980230729a0313 |
| antlr4-runtime 4.13.0 | org/antlr/antlr4-runtime/4.13.0/antlr4-runtime-4.13.0.jar | bd7f7b5d07bc0b047f10915b32ca4bb1de9e57d8049098882e4453c88c076a5d |
| jakarta.inject-api 2.0.1 | jakarta/inject/jakarta.inject-api/2.0.1/jakarta.inject-api-2.0.1.jar | f7dc98062fccf14126abb751b64fab12c312566e8cbdc8483598bffcea93af7c |
| jakarta.xml.bind-api 4.0.0 | jakarta/xml/bind/jakarta.xml.bind-api/4.0.0/jakarta.xml.bind-api-4.0.0.jar | 57e3796ad5753640088f5f9d3c53c183f2c250b7dad90529ea3e19a5515aa122 |
| jakarta.activation-api 2.1.0 | jakarta/activation/jakarta.activation-api/2.1.0/jakarta.activation-api-2.1.0.jar | 56e8d994095fe49c28138c60291482f66f18d12ac2b720e938697dce6a3135c7 |
| jaxb-runtime 4.0.2 | org/glassfish/jaxb/jaxb-runtime/4.0.2/jaxb-runtime-4.0.2.jar | 1bc271e61b71ca4bd89eb053f3d2c91d478211b02a8982cb520f216fe0e9a939 |
| jaxb-core 4.0.2 | org/glassfish/jaxb/jaxb-core/4.0.2/jaxb-core-4.0.2.jar | d7ff2954ad78480bbab9391cccff3a22f42a82b6e09aeca1c7d502411c470ccd |
| txw2 4.0.2 | org/glassfish/jaxb/txw2/4.0.2/txw2-4.0.2.jar | ea71912e4f0a42530f77c9840ae90019c46402dedfdf007cff03797429a0cf0c |
| angus-activation 2.0.0 | org/eclipse/angus/angus-activation/2.0.0/angus-activation-2.0.0.jar | 3a12d321a0f35aa9458ff9b6ee93a3de76b78e3f18b077c81721473d83079147 |
| istack-commons-runtime 4.1.1 | com/sun/istack/istack-commons-runtime/4.1.1/istack-commons-runtime-4.1.1.jar | 7e8148c5bf5d5ae6f8c4534c1873f82e80bf7f9164fd09ee573df0013918dcd3 |

### r2dbc module — R2DBC 1.0 SPI + r2dbc-h2 over in-memory H2 2.1.214

| artifact | maven-path | sha256 |
|:--|:--|:--|
| r2dbc-spi 1.0.0.RELEASE | io/r2dbc/r2dbc-spi/1.0.0.RELEASE/r2dbc-spi-1.0.0.RELEASE.jar | a5846c59fea336431a4ae72ca14edbf5299b78486fa308eafb383f4ae0ea74e5 |
| r2dbc-h2 1.0.0.RELEASE | io/r2dbc/r2dbc-h2/1.0.0.RELEASE/r2dbc-h2-1.0.0.RELEASE.jar | 747a7ba0c34da6464fc2a50d89200b4475c310a376ec9b322f9594e25033ca49 |
| h2 2.1.214 | com/h2database/h2/2.1.214/h2-2.1.214.jar | d623cdc0f61d218cf549a8d09f1c391ff91096116b22e2475475fce4fbe72bd0 |
| reactor-core 3.6.11 | io/projectreactor/reactor-core/3.6.11/reactor-core-3.6.11.jar | 14d81b2a3c0343ad532dec6268d56bd991c57fc426506d69810105e3d1c8abe2 |
| reactive-streams 1.0.4 | org/reactivestreams/reactive-streams/1.0.4/reactive-streams-1.0.4.jar | f75ca597789b3dac58f61857b9ac2e1034a68fa672db35055a8fb4509e325f28 |

> The r2dbc module deliberately pins **h2 2.1.214** (not the newer 2.2.x used elsewhere):
> `r2dbc-h2` 1.0.0.RELEASE is compiled against H2 2.1.x internal APIs and is the exact version
> the fat `r2dbc-demo.jar` bundled.

## Source → carpet-class mapping (compiled in-prebuild into `carpets.jar`)

`javac --release 17 -cp <all deps> programs/carpets/*.java` (host javac; `--release 17`
bytecode is identical for every target arch, so `carpets.jar` is cached and reused).

| source | class | runtime deps (run-jweb.sh classpath) | marker |
|:--|:--|:--|:--|
| programs/carpets/JettyCarpet.java | org.starry.dod.JettyCarpet | jetty-server/http/io/util + jetty-jakarta-servlet-api + slf4j-api 2.0.9 | JETTY_DONE |
| programs/carpets/NettyCarpet.java | org.starry.dod.NettyCarpet | netty-common/buffer/resolver/transport(+unix-common)/codec/codec-http/handler | NETTY_DONE |
| programs/carpets/MyBatisCarpet.java | org.starry.dod.MyBatisCarpet (+ User, UserMapper) | mybatis + ognl + javassist + sqlite-jdbc + slf4j-api/simple 2.0.13 | MYBATIS_DONE |
| programs/carpets/HibernateCarpet.java | org.starry.dod.HibernateCarpet | hibernate-core + community-dialects + commons-annotations + jakarta.persistence/transaction/inject + jboss-logging + jandex + classmate + byte-buddy + antlr4-runtime + jakarta.xml.bind/activation + jaxb-runtime/core/txw2 + angus-activation + istack-commons-runtime + sqlite-jdbc + slf4j-api/simple 2.0.13 | HIBERNATE_DONE |
| programs/carpets/R2dbcCarpet.java | org.starry.dod.R2dbcCarpet | r2dbc-spi + r2dbc-h2 + h2 2.1.214 + reactor-core + reactive-streams | R2DBC_DONE |
| programs/carpets/WarCarpet.java | org.starry.dod.WarCarpet | jetty-server/http/io/util + jetty-jakarta-servlet-api + slf4j-api 2.0.9 (compiles servlets in-process with the JDK `javax.tools` compiler, servlet-api on the compiler classpath) | WAR_DONE |

All six sources are in package `org.starry.dod`; `MyBatisCarpet.java` also declares the
package-private helper classes `User` and `UserMapper`. They are compiled together (the full
dep set on the classpath) but each RUNS with only its own module's curated classpath, so there
is never a duplicate SLF4J binding, servlet-api, or H2 on any one classpath.

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
  merged `java-lang` app stages for its riscv64 JDK23 cell (`stage_real_glibc_rv`), and is
  identical to the merged `java-jse` case. BellSoft Liberica generic-glibc riscv64 and the Debian
  apt `openjdk-17-jdk-headless:riscv64` build are equivalent downloadable sources of the same
  JDK17 (override via `JDK17_RISCV_URL` / `JDK17_RISCV_SHA`).

## sqlite-jdbc native JNI (`libsqlitejdbc.so`), per arch (MyBatis + Hibernate)

- **x86_64 / aarch64**: the fetched `sqlite-jdbc-3.46.1.3.jar` BUNDLES a musl JNI at
  `org/sqlite/native/Linux-Musl/{x86_64,aarch64}/libsqlitejdbc.so`; the driver self-extracts and
  `dlopen`s it at run time. Nothing is staged, nothing is committed.
- **riscv64**: the rv64 JDK17 is the prebuilt **glibc** build, so the matching JNI is the jar's
  **own bundled glibc riscv64 native** at `org/sqlite/native/Linux/riscv64/libsqlitejdbc.so`
  (its only external NEEDED are `libm.so.6` + `libc.so.6`, both in the staged Debian glibc
  closure). `prebuild.sh` extracts it from the already-fetched, sha256-pinned jar — no extra
  download and no cross-build — and stages it at `/root/jweb/native/libsqlitejdbc.so`;
  `run-jweb.sh` points `org.sqlite.lib.path` at it. Byte-identical to the merged **java-jse** case.
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
  `/root/jweb/native/libsqlitejdbc.so`; `run-jweb.sh` points `org.sqlite.lib.path` at it. Its own
  sha256 is **not** pinned (it varies with the cross-gcc version) — reproducibility is anchored on
  the two sha256-pinned source inputs + the fixed recipe. A developer who has already built it can
  seed the cache (`$JAVA_DL_ROOT/sqlitejdbc-native/loongarch64/libsqlitejdbc.so`) to skip the
  ~1-min compile. The JNI is provisioned on all four arches; if the loong musl cross-toolchain is
  genuinely unavailable, `prebuild.sh` **fails hard** (no skip path). `run-jweb.sh` always runs and
  counts all six carpets — a missing or unloadable JNI is a real FAIL, never a documented skip. The
  jetty / netty / r2dbc / war carpets do not use sqlite.

## Version-pinning notes (pom-less shaded deps)

The fat demo jars were assembled with the maven-shade plugin, which kept `pom.properties` for
most bundled deps but not for a few. Those were pinned by extracting the bundled `.class` files
and finding the Maven Central release whose classes are **byte-identical**:

- **hibernate-core / hibernate-community-dialects 6.4.4.Final** — the bundled transitive
  `byte-buddy` is 1.14.11, which hibernate-core first declares at 6.4.4.Final (6.4.2/6.4.3 use
  1.14.7); all 6733 shared `org/hibernate/**` classes are byte-identical to 6.4.4.Final (0
  differing; later 6.4.x patch levels diverge). `SQLiteDialect` lives in
  hibernate-community-dialects; `hibernate-commons-annotations` 6.0.6.Final supplies the
  `org/hibernate/annotations/common/**` classes.
- **jetty-jakarta-servlet-api 5.0.2** — `jakarta.servlet:jakarta.servlet-api` has no 5.0.2
  (only 5.0.0); jetty 11 uses `org.eclipse.jetty.toolchain:jetty-jakarta-servlet-api`, and
  jetty-server 11.0.21 resolves 5.0.2. All 85 bundled `jakarta/servlet/**` classes are
  byte-identical to toolchain 5.0.2 (5.0.1 is also byte-identical; 5.0.2 is the version
  jetty 11.0.21 pins).
- **h2 2.1.214, reactor-core 3.6.11, reactive-streams 1.0.4** — `Constants` / `Flux` /
  `Publisher` classes byte-match these exact releases (reactor-core 3.6.11's Maven upload
  timestamp 2024-10-15 06:42 matches the bundled `Flux.class` timestamp 2024-10-15 06:41).
