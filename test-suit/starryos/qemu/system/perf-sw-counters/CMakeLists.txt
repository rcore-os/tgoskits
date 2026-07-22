cmake_minimum_required(VERSION 3.20)
project(perf-sw-counters C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Opens the five PERF_TYPE_SOFTWARE default `perf stat` rows as per-task counters,
# runs a workload that generates each, and asserts read(perf_fd) returns a real
# count (not <not counted>). Software events are pure accounting, so this is not
# arch-gated; it skips-as-pass only if perf_event_open is unwired (ENOSYS).
add_executable(perf-sw-counters src/perf_sw_counters.c)
target_compile_options(perf-sw-counters PRIVATE -Wall -Wextra -Werror -O0)
install(TARGETS perf-sw-counters RUNTIME DESTINATION usr/bin/starry-test-suit)
