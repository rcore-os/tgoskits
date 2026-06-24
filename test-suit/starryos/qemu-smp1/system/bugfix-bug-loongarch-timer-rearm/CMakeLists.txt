cmake_minimum_required(VERSION 3.20)
project(test-loongarch-timer-rearm C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(
    test-loongarch-timer-rearm
    "loongarch64"
    "test-loongarch-timer-rearm skipped on unsupported architecture"
    src/main.c
)
target_include_directories(test-loongarch-timer-rearm PRIVATE ../common)
target_compile_options(test-loongarch-timer-rearm PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-loongarch-timer-rearm RUNTIME DESTINATION usr/bin/starry-test-suit)
