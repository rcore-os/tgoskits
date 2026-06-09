cmake_minimum_required(VERSION 3.20)
project(test-openat-umask-smp C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
include("${CMAKE_CURRENT_LIST_DIR}/../../common/starry_arch_filter.cmake")
starry_arch_filtered_executable(
    test-openat-umask-smp
    "riscv64"
    "test-openat-umask-smp skipped on non-riscv64 qemu-smp4"
    src/main.c)
target_compile_options(test-openat-umask-smp PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-openat-umask-smp RUNTIME DESTINATION usr/bin/starry-test-suit)
