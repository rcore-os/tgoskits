# BackCompatReal — real-world Java-8 forward-compatibility suite

This is the **real** backward-compatibility leg of the `java-lang` carpet. Unlike
`programs/BackCompat.java` (a small JDK17-bytecode self-test), `BackCompatReal`
proves the JVM's *forward-compatibility* guarantee against **real third-party
libraries** compiled to **Java 8 bytecode (class-file major version 52)**: a jar
built with `javac --release 8` must run **unchanged** on JDK 17 / 21 / 23 / 25.

## What it is

- `src/BackCompatReal.java` — the JUnit runner `main()`. Drives the 5 per-library
  `*BackCompatTest` classes through `JUnitCore` and prints, on zero failures,
  exactly `BACKCOMPAT_REAL_OK 299` (the token the gate asserts), or
  `BACKCOMPAT_REAL_FAIL` on any failure.
- `src/CommonsBackCompatTest.java` — Apache Commons IO 2.11.0 / Math3 3.6.1 /
  Lang3 3.12.0 / Collections4 4.4.
- `src/LoggingBackCompatTest.java` — Log4j2 (api + core) 2.17.1.
- `src/SqlBackCompatTest.java` — H2 2.1.214 + HSQLDB 2.5.2 (embedded JDBC).
- `src/JsonBackCompatTest.java` — Gson 2.10.1.
- `src/ScriptBackCompatTest.java` — BeanShell (bsh) 2.0b6.

299 JUnit tests total, host-verified, deterministic.

## Compiled `--release 8` = bytecode 52

The suite is compiled with `--release 8` so the emitted class files are **major
version 52** (Java 8). That is the whole point: a Java-8 jar that runs on JDK
17/21/23/25 is the cross-version backward-compat proof. `prebuild.sh`
`stage_backcompat()` recompiles `src/*.java` on the host with
`javac --release 8 -cp libs/*` when a host `javac` is available (reproducible
build), else copies the prebuilt `backcompat-real.jar`. The 12 dependency jars are
fetched from Maven Central by sha256 (or copied from the local cache) and staged,
together with the jar, into the overlay at `/root/bcreal/{libs,backcompat-real.jar}`.

## Library coordinates (Maven Central) + sha256

| jar | Maven coordinate | sha256 |
| :-- | :-- | :-- |
| commons-io-2.11.0.jar | commons-io:commons-io:2.11.0 | 961b2f6d87dbacc5d54abf45ab7a6e2495f89b75598962d8c723cea9bc210908 |
| commons-math3-3.6.1.jar | org.apache.commons:commons-math3:3.6.1 | 1e56d7b058d28b65abd256b8458e3885b674c1d588fa43cd7d1cbb9c7ef2b308 |
| commons-lang3-3.12.0.jar | org.apache.commons:commons-lang3:3.12.0 | d919d904486c037f8d193412da0c92e22a9fa24230b9d67a57855c5c31c7e94e |
| commons-collections4-4.4.jar | org.apache.commons:commons-collections4:4.4 | 1df8b9430b5c8ed143d7815e403e33ef5371b2400aadbe9bda0883762e0846d1 |
| log4j-api-2.17.1.jar | org.apache.logging.log4j:log4j-api:2.17.1 | b0d8a4c8ab4fb8b1888d0095822703b0e6d4793c419550203da9e69196161de4 |
| log4j-core-2.17.1.jar | org.apache.logging.log4j:log4j-core:2.17.1 | c967f223487980b9364e94a7c7f9a8a01fd3ee7c19bdbf0b0f9f8cb8511f3d41 |
| h2-2.1.214.jar | com.h2database:h2:2.1.214 | d623cdc0f61d218cf549a8d09f1c391ff91096116b22e2475475fce4fbe72bd0 |
| hsqldb-2.5.2.jar | org.hsqldb:hsqldb:2.5.2 | e4aa39c5afb318e8effdec80a0e6de7c9dacc453c1cf7666c515f29a16658dac |
| gson-2.10.1.jar | com.google.code.gson:gson:2.10.1 | 4241c14a7727c34feea6507ec801318a3d4a90f070e4525681079fb94ee4c593 |
| bsh-2.0b6.jar | org.beanshell:bsh:2.0b6 | a17955976070c0573235ee662f2794a78082758b61accffce8d3f8aedcd91047 |
| junit-4.13.2.jar | junit:junit:4.13.2 | 8e495b634469d64fb8acfa3495a065cbacc8a0fff55ce1e31007be4c16dc57d3 |
| hamcrest-core-1.3.jar | org.hamcrest:hamcrest-core:1.3 | 66fdef91e9739348df7a096aa384a5685f4e875584cce89386a7a47251c4d8e9 |
