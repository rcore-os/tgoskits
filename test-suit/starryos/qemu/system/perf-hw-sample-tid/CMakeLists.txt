cmake_minimum_required(VERSION 3.20)
project(perf-hw-sample-tid C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# A worker (non-leader) thread opens a per-task sampling event on itself and
# checks every PERF_RECORD_SAMPLE reports the process tgid as pid and the
# worker gettid() as tid. Needs pthread; keep frame pointers so the read()
# workload's samples parse cleanly. main() skips as a pass off aarch64.
add_executable(perf-hw-sample-tid src/perf_hw_sample_tid.c)
target_compile_options(perf-hw-sample-tid PRIVATE
    -Wall
    -Wextra
    -Werror
    -O0
    -fno-omit-frame-pointer
)
target_link_libraries(perf-hw-sample-tid PRIVATE pthread)
install(TARGETS perf-hw-sample-tid RUNTIME DESTINATION usr/bin/starry-test-suit)
