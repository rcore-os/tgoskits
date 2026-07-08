#!/bin/sh
# run-jweb.sh — on-target gate for the StarryOS java-web JEE framework carpet.
#
# Staged into the rootfs by prebuild.sh and invoked as the ENTIRE shell_init_cmd
# (`sh /usr/bin/run-jweb.sh`). The gate lives in a staged script, not inline in the toml, so
# the harness does not echo a literal `TEST PASSED` back over the serial console and
# self-match success_regex: TEST PASSED is printed ONLY by this script's real stdout, ONLY
# when all six modules passed (PASS==TOTAL==6).
#
# The MyBatis + Hibernate carpets run over sqlite-jdbc; every arch provisions its native JNI and
# RUNS them: x86_64/aarch64 self-extract the jar's musl JNI, riscv64 loads the jar's bundled glibc
# JNI under its glibc JDK17, and loongarch64 loads the musl JNI that prebuild.sh cross-compiles
# in-prebuild from xerial/sqlite-jdbc's official source. All six carpets are counted; a missing or
# unloadable sqlite-jdbc JNI is a real FAIL, never a skip. The jetty / netty / r2dbc / war carpets
# never use sqlite and always run.
#
# CLASSPATH MODEL: prebuild.sh compiles the six carpet classes into $D/carpets.jar and stages
# the fetched Maven dependency jars under $D/libs/. Each carpet runs with exactly its own
# framework module's dependency jars on the classpath (the same grouping the previous fat
# "demo jars" bundled). Keeping the classpaths separate avoids ever having two SLF4J bindings
# or two H2 / servlet-api versions on one classpath.
#
# Each module is an industrial-grade carpet exercising the full public API surface of one
# JEE/JVM framework (dozens-to-hundreds of exact-value assertions per module; HTTP servers are
# driven over real IPv4 loopback with HttpURLConnection, ORMs run against an in-memory DB),
# terminated by an anchored *_DONE marker that is printed only when its own fail count is zero.
set -u

case "$(uname -m)" in
  x86_64)      A=x86_64 ;;
  aarch64)     A=aarch64 ;;
  riscv64)     A=riscv64 ;;
  loongarch64) A=loongarch64 ;;
  *)           A="$(uname -m)" ;;
esac

JH=/opt/jdk17
# musl JDK arches (x86_64/aarch64/loongarch64) resolve libjvm.so via the musl loader path.
# riscv64's JDK17 is the prebuilt GLIBC build: it is loaded by its OWN ld-linux interp
# (/lib/ld-linux-riscv64-lp64d.so.1) + the staged Debian glibc closure and finds its own libs via
# $ORIGIN rpath, so it ignores this file — written harmlessly for the shared code path.
printf '/lib\n/usr/lib\n%s/lib\n%s/lib/server\n' "$JH" "$JH" > "/etc/ld-musl-$A.path"
export JAVA_HOME="$JH" PATH="$JH/bin:$PATH"

# StarryOS JIT is still unstable -> force the interpreter on every JVM.
J="$JH/bin/java -Xint -Xms32m -Xmx512m"
D=/root/jweb
L=$D/libs

# Per-module classpaths (== the original fat-jar contents; carpets.jar holds the compiled
# carpet classes, each module's third-party jars come from $L).
JETTY_CP="$D/carpets.jar:$L/jetty-server-11.0.21.jar:$L/jetty-http-11.0.21.jar:$L/jetty-io-11.0.21.jar:$L/jetty-util-11.0.21.jar:$L/jetty-jakarta-servlet-api-5.0.2.jar:$L/slf4j-api-2.0.9.jar"
NETTY_CP="$D/carpets.jar:$L/netty-common-4.1.112.Final.jar:$L/netty-buffer-4.1.112.Final.jar:$L/netty-resolver-4.1.112.Final.jar:$L/netty-transport-4.1.112.Final.jar:$L/netty-transport-native-unix-common-4.1.112.Final.jar:$L/netty-codec-4.1.112.Final.jar:$L/netty-codec-http-4.1.112.Final.jar:$L/netty-handler-4.1.112.Final.jar"
MYBATIS_CP="$D/carpets.jar:$L/mybatis-3.5.16.jar:$L/ognl-3.4.2.jar:$L/javassist-3.30.2-GA.jar:$L/sqlite-jdbc-3.46.1.3.jar:$L/slf4j-api-2.0.13.jar:$L/slf4j-simple-2.0.13.jar"
HIBERNATE_CP="$D/carpets.jar:$L/hibernate-core-6.4.4.Final.jar:$L/hibernate-community-dialects-6.4.4.Final.jar:$L/hibernate-commons-annotations-6.0.6.Final.jar:$L/jakarta.persistence-api-3.1.0.jar:$L/jakarta.transaction-api-2.0.1.jar:$L/jboss-logging-3.5.0.Final.jar:$L/jandex-3.1.2.jar:$L/classmate-1.5.1.jar:$L/byte-buddy-1.14.11.jar:$L/antlr4-runtime-4.13.0.jar:$L/jakarta.inject-api-2.0.1.jar:$L/jakarta.xml.bind-api-4.0.0.jar:$L/jakarta.activation-api-2.1.0.jar:$L/jaxb-runtime-4.0.2.jar:$L/jaxb-core-4.0.2.jar:$L/txw2-4.0.2.jar:$L/angus-activation-2.0.0.jar:$L/istack-commons-runtime-4.1.1.jar:$L/sqlite-jdbc-3.46.1.3.jar:$L/slf4j-api-2.0.13.jar:$L/slf4j-simple-2.0.13.jar"
R2DBC_CP="$D/carpets.jar:$L/r2dbc-spi-1.0.0.RELEASE.jar:$L/r2dbc-h2-1.0.0.RELEASE.jar:$L/h2-2.1.214.jar:$L/reactor-core-3.6.11.jar:$L/reactive-streams-1.0.4.jar"

# sqlite-jdbc native JNI (per arch), used by MyBatis + Hibernate:
#   x86_64/aarch64 : the driver self-extracts the jar's bundled Linux-Musl JNI (nothing staged).
#   riscv64        : the glibc JDK17 loads the jar's bundled GLIBC riscv64 JNI staged by prebuild
#                    at $D/native (matches the glibc JDK + the Debian glibc closure).
#   loongarch64    : prebuild cross-compiles a musl loong JNI in-prebuild from official source and
#                    stages it at $D/native.
# All four arches provision the JNI, so MyBatis + Hibernate always RUN and are counted; a missing
# or unloadable JNI is a real FAIL (surfaced by `run`), never a skip.
SQLP=""
case "$A" in
    riscv64|loongarch64)
        if [ -f "$D/native/libsqlitejdbc.so" ]; then
            SQLP="-Dorg.sqlite.lib.path=$D/native -Dorg.sqlite.lib.name=libsqlitejdbc.so"
        fi ;;
esac

PASS=0
TOTAL=0
# strict count: a build/env that skips a module leaves TOTAL < the full set and must FAIL,
# not vacuously pass on TOTAL > 0.
EXPECTED_MODULES=6
run() { # run <name> <marker> <cmd...>
    name="$1"; marker="$2"; shift 2
    TOTAL=$((TOTAL + 1))
    "$@" > "/tmp/$name.out" 2>&1
    if grep -aq "$marker" "/tmp/$name.out" 2>/dev/null; then
        echo "  OK   $name ($marker)"
        PASS=$((PASS + 1))
    else
        echo "  FAIL $name ($marker)"
        grep -aiE 'exception|error|fail|caused by|bind|address' "/tmp/$name.out" | tail -6
    fi
}

echo "=== java-web: JEE framework carpets (jetty/netty | mybatis/hibernate/r2dbc | war) ==="
run jetty     JETTY_DONE     $J -cp "$JETTY_CP"     org.starry.dod.JettyCarpet
run netty     NETTY_DONE     $J -cp "$NETTY_CP"     org.starry.dod.NettyCarpet
run mybatis   MYBATIS_DONE   $J $SQLP -cp "$MYBATIS_CP"   org.starry.dod.MyBatisCarpet
run hibernate HIBERNATE_DONE $J $SQLP -cp "$HIBERNATE_CP" org.starry.dod.HibernateCarpet
run r2dbc     R2DBC_DONE     $J -cp "$R2DBC_CP"     org.starry.dod.R2dbcCarpet
run war       WAR_DONE       $J -cp "$JETTY_CP"     org.starry.dod.WarCarpet

echo "AGGREGATE: PASS=$PASS TOTAL=$TOTAL"
if [ "$PASS" = "$TOTAL" ] && [ "$TOTAL" -eq "$EXPECTED_MODULES" ]; then
    echo "JAVA_WEB_OK=$PASS/$TOTAL"
    echo "TEST PASSED"
    exit 0
fi
echo "TEST FAILED"
exit 1
