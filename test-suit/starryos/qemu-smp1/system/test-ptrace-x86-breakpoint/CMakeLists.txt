cmake_minimum_required(VERSION 3.20)
project(test-ptrace-x86-breakpoint C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(test-ptrace-x86-breakpoint "x86_64" "test-ptrace-x86-breakpoint skipped on unsupported architecture" src/main.c)
target_compile_options(test-ptrace-x86-breakpoint PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-ptrace-x86-breakpoint RUNTIME DESTINATION usr/bin/starry-test-suit)
