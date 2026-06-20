cmake_minimum_required(VERSION 3.20)
project(test-ptrace-listen C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(test-ptrace-listen "riscv64" "test-ptrace-listen skipped on unsupported architecture" src/main.c)
target_include_directories(test-ptrace-listen PRIVATE src)
target_compile_options(test-ptrace-listen PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-ptrace-listen RUNTIME DESTINATION usr/bin/starry-test-suit)
