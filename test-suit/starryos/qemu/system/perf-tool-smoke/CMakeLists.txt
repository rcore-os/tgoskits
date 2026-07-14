cmake_minimum_required(VERSION 3.20)
project(perf-tool-smoke NONE)

# Prebuilt upstream linux-6.1 `perf`, statically linked for aarch64-musl. Proves
# the real perf binary runs on StarryOS via the Linux-compatible
# perf_event_open(2) ABI. The binary is a committed, reproducible test asset built
# from the pinned recipe in `build-perf-libelf.sh` (native aarch64 Alpine musl,
# static, +libelf for user-symbol names). It is committed (not gitignored) so this
# case runs deterministically in CI instead of being silently skipped when a local
# build is absent — a missing binary is a hard error, never a skipped subcase.
#
# The asset is an aarch64 ELF, so it can ONLY execute on aarch64 targets. We
# install-and-run it exclusively on aarch64; on every other target architecture we
# install an explicit skip stub (renamed perf-tool-smoke) so the grouped runner
# records a visible SKIP instead of executing a foreign ELF (which SIGSEGVs) —
# never a silent drop, never a heterogeneous binary in usr/bin.
if(CMAKE_SYSTEM_PROCESSOR STREQUAL "aarch64")
    set(PERF_BIN "${CMAKE_CURRENT_SOURCE_DIR}/perf")
    if(NOT EXISTS "${PERF_BIN}")
        message(FATAL_ERROR
            "perf-tool-smoke: prebuilt perf binary missing at ${PERF_BIN}. It is a "
            "committed test asset; if you removed it, restore it or rebuild via "
            "build-perf-libelf.sh. Refusing to silently skip perf coverage.")
    endif()

    # Provenance gate: the committed binary must match the SHA-256 recorded in
    # perf.sha256 (emitted by build-perf-libelf.sh). This makes the asset's
    # identity verifiable — a tampered or accidentally-replaced binary fails the
    # build instead of shipping silently.
    file(READ "${CMAKE_CURRENT_SOURCE_DIR}/perf.sha256" PERF_SHA_RAW)
    string(REGEX MATCH "[0-9a-fA-F]+" PERF_SHA_EXPECTED "${PERF_SHA_RAW}")
    file(SHA256 "${PERF_BIN}" PERF_SHA_ACTUAL)
    if(NOT PERF_SHA_ACTUAL STREQUAL PERF_SHA_EXPECTED)
        message(FATAL_ERROR
            "perf-tool-smoke: perf binary SHA-256 mismatch.\n"
            "  expected (perf.sha256): ${PERF_SHA_EXPECTED}\n"
            "  actual   (perf):        ${PERF_SHA_ACTUAL}\n"
            "The committed asset does not match its recorded provenance; rebuild "
            "via build-perf-libelf.sh and update perf.sha256, or restore the asset.")
    endif()

    install(PROGRAMS "${PERF_BIN}" DESTINATION usr/bin)
    install(PROGRAMS src/perf-tool-smoke.sh
        DESTINATION usr/bin/starry-test-suit
        RENAME perf-tool-smoke
    )
else()
    install(PROGRAMS src/perf-tool-smoke-skip.sh
        DESTINATION usr/bin/starry-test-suit
        RENAME perf-tool-smoke
    )
endif()
