cmake_minimum_required(VERSION 3.20)
project(bug-sched-affinity-pid C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)
include("${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake")
starry_arch_filtered_executable(
    bug-sched-affinity-pid
    "^(aarch64|riscv64|x86_64)"
    "bug-sched-affinity-pid skipped on loongarch64 qemu-smp4"
    src/main.c)
target_compile_options(bug-sched-affinity-pid PRIVATE -Wall -Wextra -Werror)
install(TARGETS bug-sched-affinity-pid RUNTIME DESTINATION usr/bin/starry-test-suit)
