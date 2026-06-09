cmake_minimum_required(VERSION 3.20)
project(test-user-backtrace C)
include(${CMAKE_CURRENT_LIST_DIR}/../../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(test-user-backtrace "riscv64" "test-user-backtrace skipped on unsupported architecture" src/main.c)
target_compile_options(test-user-backtrace PRIVATE
    -Wall
    -Wextra
    -Werror
    -O0
    -fno-omit-frame-pointer
    -fno-optimize-sibling-calls
)

install(TARGETS test-user-backtrace RUNTIME DESTINATION usr/bin/starry-test-suit)
