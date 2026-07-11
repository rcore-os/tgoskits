cmake_minimum_required(VERSION 3.20)
project(perf-hw-lost C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Overflows a tiny sampling ring, drains between bursts, and asserts an in-band
# PERF_RECORD_LOST record with a non-zero lost count is emitted. Hardware PMU
# (ARM PMUv3), so main() skips-as-pass off aarch64.
add_executable(perf-hw-lost src/perf_hw_lost.c)
target_compile_options(perf-hw-lost PRIVATE -Wall -Wextra -Werror -O0)
install(TARGETS perf-hw-lost RUNTIME DESTINATION usr/bin/starry-test-suit)
