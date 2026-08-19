cmake_minimum_required(VERSION 3.20)
project(perf-validate-smp8 C)

# Board session-asset build for the FULL big.LITTLE enforcement case.
#
# This case runs the SAME validator as the smp1 anchor next door — only the
# KERNEL build differs (this case's build wrapper is max_cpu_num=8), the
# userspace validator binary is identical. To keep a single board-side source of
# truth it compiles the anchor's committed C source directly rather than holding
# a byte-identical copy. That is safe here because the board session-asset build
# fully resets its build/overlay directories and cross-compiles from source on
# every run (no cached-artifact reuse), so there is no stale-serving concern —
# the reason the QEMU grouped-C harness keeps a local copy does not apply.
#
# Built + uploaded per run and executed via `${sessionFile:bin/perf-validate}`;
# `-static` keeps the ELF self-contained (StarryOS loads static ELFs).
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

set(PERF_VALIDATE_SRC
    "${CMAKE_CURRENT_LIST_DIR}/../../perf-validate/c/src/perf_validate.c")
if(NOT EXISTS "${PERF_VALIDATE_SRC}")
    message(FATAL_ERROR
        "shared validator source missing: ${PERF_VALIDATE_SRC} "
        "(the smp1 anchor case ../perf-validate owns it)")
endif()

add_executable(perf-validate "${PERF_VALIDATE_SRC}")
target_compile_options(perf-validate PRIVATE -Wall -Wextra -Werror)
target_link_options(perf-validate PRIVATE -static)

# Uploaded and downloaded as `${sessionFile:bin/perf-validate}`.
install(TARGETS perf-validate RUNTIME DESTINATION bin)
