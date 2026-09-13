cmake_minimum_required(VERSION 3.20)
project(starrynixos-stage2 C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)
starry_arch_filtered_executable(
    starrynixos-stage2
    "x86_64"
    "StarryNixOS stage-2 baseline probes are x86_64-only"
    src/main.c
)
target_compile_options(starrynixos-stage2 PRIVATE -Wall -Wextra -Werror)
install(TARGETS starrynixos-stage2 RUNTIME DESTINATION usr/bin/starry-test-suit)
