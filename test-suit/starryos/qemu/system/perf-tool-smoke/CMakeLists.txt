cmake_minimum_required(VERSION 3.20)
project(perf-tool-smoke NONE)

# Prebuilt upstream linux-6.1 `perf`, statically linked for aarch64-musl. Proves
# the real perf binary runs on StarryOS via the Linux-compatible
# perf_event_open(2) ABI. The binary is a committed, reproducible test asset built
# from the pinned recipe in `build-perf-libelf.sh` (native aarch64 Alpine musl,
# static, +libelf for user-symbol names). It is committed (not gitignored) so this
# case runs deterministically in CI instead of being silently skipped when a local
# build is absent — a missing binary is a hard error, never a skipped subcase.
set(PERF_BIN "${CMAKE_CURRENT_SOURCE_DIR}/perf")
if(NOT EXISTS "${PERF_BIN}")
    message(FATAL_ERROR
        "perf-tool-smoke: prebuilt perf binary missing at ${PERF_BIN}. It is a "
        "committed test asset; if you removed it, restore it or rebuild via "
        "build-perf-libelf.sh. Refusing to silently skip perf coverage.")
endif()

install(PROGRAMS "${PERF_BIN}" DESTINATION usr/bin)
install(PROGRAMS src/perf-tool-smoke.sh
    DESTINATION usr/bin/starry-test-suit
    RENAME perf-tool-smoke
)
