cmake_minimum_required(VERSION 3.20)
project(perf-validate C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Local src copy (the grouped-C harness detects changes by the subcase dir's own
# contents, so a cross-dir source reference would be served stale). Under QEMU
# (homogeneous cortex-a53) the binary auto-enters parity-override SELFTEST mode
# and exits 0 — a permanent regression smoke of the board validator's logic.
add_executable(perf-validate src/perf_validate.c)
target_compile_options(perf-validate PRIVATE -Wall -Wextra -Werror)
install(TARGETS perf-validate RUNTIME DESTINATION usr/bin/starry-test-suit)
