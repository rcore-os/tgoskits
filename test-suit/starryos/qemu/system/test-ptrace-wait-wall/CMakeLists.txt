cmake_minimum_required(VERSION 3.20)
project(test-ptrace-wait-wall C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

starry_arch_filtered_executable(test-ptrace-wait-wall "riscv64" "test-ptrace-wait-wall skipped on unsupported architecture" src/main.c)
target_compile_options(test-ptrace-wait-wall PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-ptrace-wait-wall RUNTIME DESTINATION usr/bin/starry-test-suit)
