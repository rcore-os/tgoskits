cmake_minimum_required(VERSION 3.20)
project(perf-hw-cache C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Opens the common PERF_TYPE_HW_CACHE combinations (L1-dcache-load-misses, LLC-*,
# dTLB/iTLB-misses, branch-misses) and asserts each opens + reads, while an
# unsupported PREFETCH combination is rejected. Hardware PMU (ARM PMUv3), so
# main() skips-as-pass off aarch64.
add_executable(perf-hw-cache src/perf_hw_cache.c)
target_compile_options(perf-hw-cache PRIVATE -Wall -Wextra -Werror -O0)
install(TARGETS perf-hw-cache RUNTIME DESTINATION usr/bin/starry-test-suit)
