cmake_minimum_required(VERSION 3.20)
project(perf-hw-sample-read C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Opens a sampling event with PERF_SAMPLE_READ (read_format 0) and checks each
# sample carries a strictly-increasing running count. Hardware PMU (ARM PMUv3);
# main() skips as a pass off aarch64. Uses a read(/dev/zero) workload so the
# QEMU-TCG cycle counter overflows steadily.
add_executable(perf-hw-sample-read src/perf_hw_sample_read.c)
target_compile_options(perf-hw-sample-read PRIVATE
    -Wall -Wextra -Werror -O0 -fno-omit-frame-pointer)
install(TARGETS perf-hw-sample-read RUNTIME DESTINATION usr/bin/starry-test-suit)
