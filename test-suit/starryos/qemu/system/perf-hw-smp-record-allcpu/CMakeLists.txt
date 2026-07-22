cmake_minimum_required(VERSION 3.20)
project(perf-hw-smp-record-allcpu C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# `perf record -a` per-CPU SAMPLING fan-out: one system-wide sampling event per
# cpu (pid=-1, cpu=i) must arm its PMU + overflow IRQ ON core i (not the opening
# core), so each core's overflow writes PERF_RECORD_SAMPLE into that event's ring
# stamped cpu=i. Forks a read(/dev/zero) child pinned to each cpu, opens+mmaps a
# sampling event per cpu, and asserts every ring gets >=1 sample all tagged with
# its own cpu. Hardware PMU (ARM PMUv3); main() skips as a pass off aarch64.
add_executable(perf-hw-smp-record-allcpu src/perf_hw_smp_record_allcpu.c)
target_compile_options(perf-hw-smp-record-allcpu PRIVATE -Wall -Wextra -Werror)
install(TARGETS perf-hw-smp-record-allcpu RUNTIME DESTINATION usr/bin/starry-test-suit)
