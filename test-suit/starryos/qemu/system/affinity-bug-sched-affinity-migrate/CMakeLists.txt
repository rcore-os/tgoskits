cmake_minimum_required(VERSION 3.20)
project(bug-sched-affinity-migrate C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)
include("${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake")
starry_arch_filtered_executable(
    bug-sched-affinity-migrate
    "^(aarch64|x86_64)"
    "bug-sched-affinity-migrate skipped on loongarch64/riscv64 qemu"
    src/main.c)
target_compile_options(bug-sched-affinity-migrate PRIVATE -Wall -Wextra -Werror)
install(TARGETS bug-sched-affinity-migrate RUNTIME DESTINATION usr/bin/starry-test-suit)
