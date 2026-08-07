cmake_minimum_required(VERSION 3.20)
project(test-smp-thread-distribution C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

include("${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake")
# This asserts new tasks spread across >=2 online CPUs. The SMP-distribution
# change deliberately keeps new-task placement CPU-local on loongarch64 (its
# secondaries are unstable), so the assertion cannot hold there — skip it on
# loongarch64 (build a skip stub instead).
starry_arch_filtered_executable(
    test-smp-thread-distribution
    "^(aarch64|riscv64|x86_64)"
    "test-smp-thread-distribution skipped on loongarch64 qemu (new-task placement kept CPU-local)"
    src/main.c)
target_include_directories(test-smp-thread-distribution PRIVATE src)
target_compile_options(test-smp-thread-distribution PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-smp-thread-distribution PRIVATE pthread)

install(TARGETS test-smp-thread-distribution RUNTIME DESTINATION usr/bin/starry-test-suit)
