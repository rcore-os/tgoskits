cmake_minimum_required(VERSION 3.20)
project(test-proc-status-tracerpid C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

starry_arch_filtered_executable(test-proc-status-tracerpid "riscv64" "test-proc-status-tracerpid skipped on unsupported architecture" src/main.c)
target_compile_options(test-proc-status-tracerpid PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-proc-status-tracerpid RUNTIME DESTINATION usr/bin/starry-test-suit)
