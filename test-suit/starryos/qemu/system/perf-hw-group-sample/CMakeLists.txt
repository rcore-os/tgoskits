cmake_minimum_required(VERSION 3.20)
project(perf-hw-group-sample C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Group-leader sampling: a per-task sampling leader opened with
# read_format = PERF_FORMAT_GROUP | PERF_FORMAT_ID must emit the WHOLE group's
# counters (nr, then leader value+id, then each member value+id) in every
# PERF_RECORD_SAMPLE. A worker thread opens {cycles-sampler, cycles-counter} on
# itself (pid > 0), runs a read(/dev/zero) workload so the QEMU-TCG cycle counter
# overflows, and checks each sample carries the member's LIVE count read from the
# overflow handler. Hardware PMU (ARM PMUv3); main() skips as a pass off aarch64.
add_executable(perf-hw-group-sample src/perf_hw_group_sample.c)
target_link_libraries(perf-hw-group-sample pthread)
target_compile_options(perf-hw-group-sample PRIVATE
    -Wall -Wextra -Werror -O0 -fno-omit-frame-pointer)
install(TARGETS perf-hw-group-sample RUNTIME DESTINATION usr/bin/starry-test-suit)
