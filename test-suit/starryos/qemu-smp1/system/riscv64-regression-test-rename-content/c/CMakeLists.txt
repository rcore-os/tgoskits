cmake_minimum_required(VERSION 3.20)
project(test-rename-content C)
include(${CMAKE_CURRENT_LIST_DIR}/../../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(test-rename-content "riscv64" "test-rename-content skipped on unsupported architecture" src/main.c)
target_compile_options(test-rename-content PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-rename-content RUNTIME DESTINATION usr/bin/starry-test-suit)
