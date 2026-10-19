cmake_minimum_required(VERSION 3.20)
project(test-aarch64-signal-fpsimd C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# The FPSIMD signal frame is aarch64-specific: the sigcontext reservation there
# carries a Linux `struct fpsimd_context` record. x86_64 passes its fpstate
# through a separately aligned XSAVE payload, and riscv64/loongarch64 have their
# own reservation formats, so every other architecture builds the shared
# arch_skip program.
starry_arch_filtered_executable(
    test-aarch64-signal-fpsimd
    "aarch64"
    "test-aarch64-signal-fpsimd skipped on unsupported architecture"
    src/main.c
)
target_include_directories(test-aarch64-signal-fpsimd PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../common)
target_compile_options(test-aarch64-signal-fpsimd PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-aarch64-signal-fpsimd RUNTIME DESTINATION usr/bin/starry-test-suit)
