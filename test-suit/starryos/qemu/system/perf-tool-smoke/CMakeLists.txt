cmake_minimum_required(VERSION 3.20)
project(perf-tool-smoke NONE)

# Prebuilt upstream linux-6.1 `perf`, statically linked for aarch64-musl (see
# build-perf.sh). Proves the real perf binary runs on StarryOS via the
# Linux-compatible perf_event_open(2) ABI. The 2.8 MB binary is NOT committed
# (gitignored); when it is absent (e.g. CI without a local build) this case
# installs nothing and is silently skipped, so the qemu/system group still builds.
set(PERF_BIN "${CMAKE_CURRENT_SOURCE_DIR}/perf")
if(NOT EXISTS "${PERF_BIN}")
    message(STATUS "perf-tool-smoke: prebuilt perf absent; skipping (run build-perf.sh to enable)")
    return()
endif()

install(PROGRAMS "${PERF_BIN}" DESTINATION usr/bin)
install(PROGRAMS src/perf-tool-smoke.sh
    DESTINATION usr/bin/starry-test-suit
    RENAME perf-tool-smoke
)
