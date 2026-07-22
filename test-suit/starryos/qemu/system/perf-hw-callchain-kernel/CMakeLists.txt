cmake_minimum_required(VERSION 3.20)
project(perf-hw-callchain-kernel C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Plain C (no arch-specific asm), so it builds on every arch; main() skips as a
# pass on non-aarch64 since hardware-PMU sampling is ARM PMUv3 only. Frame
# pointers are kept so the kernel FP chain the test checks is well formed (they
# already are in the kernel build; this only affects the tiny test binary).
add_executable(perf-hw-callchain-kernel src/perf_hw_callchain_kernel.c)
target_compile_options(perf-hw-callchain-kernel PRIVATE
    -Wall
    -Wextra
    -Werror
    -fno-omit-frame-pointer
)
install(TARGETS perf-hw-callchain-kernel RUNTIME DESTINATION usr/bin/starry-test-suit)
