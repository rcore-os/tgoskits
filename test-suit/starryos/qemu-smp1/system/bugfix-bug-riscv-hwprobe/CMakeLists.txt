cmake_minimum_required(VERSION 3.20)

project(bug-riscv-hwprobe C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(
    bug-riscv-hwprobe
    "riscv64"
    "bug-riscv-hwprobe skipped on unsupported architecture"
    src/main.c
)
target_compile_options(bug-riscv-hwprobe PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-riscv-hwprobe RUNTIME DESTINATION usr/bin/starry-test-suit)
