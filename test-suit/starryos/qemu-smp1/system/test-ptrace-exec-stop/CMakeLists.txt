cmake_minimum_required(VERSION 3.20)
project(test-ptrace-exec-stop C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(test-ptrace-exec-stop "riscv64|x86_64" "test-ptrace-exec-stop skipped on unsupported architecture" src/main.c)
target_compile_options(test-ptrace-exec-stop PRIVATE
    -Wall
    -Wextra
    -Werror
)

install(TARGETS test-ptrace-exec-stop RUNTIME DESTINATION usr/bin/starry-test-suit)
