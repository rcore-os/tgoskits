cmake_minimum_required(VERSION 3.20)
project(perf-hw-callchain-user C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# The test workload is a known nested call chain that the kernel unwinds from
# user space, so the binary MUST keep frame pointers and real (non-tail) calls.
# -O0 + -fno-omit-frame-pointer + -fno-optimize-sibling-calls guarantees each
# outer/mid/inner/busy frame has a walkable AAPCS64 frame record. Plain C (no
# arch asm), so it builds on every arch; main() skips as a pass off aarch64.
add_executable(perf-hw-callchain-user src/perf_hw_callchain_user.c)
target_compile_options(perf-hw-callchain-user PRIVATE
    -Wall
    -Wextra
    -Werror
    -O0
    -fno-omit-frame-pointer
    -fno-optimize-sibling-calls
)
install(TARGETS perf-hw-callchain-user RUNTIME DESTINATION usr/bin/starry-test-suit)
