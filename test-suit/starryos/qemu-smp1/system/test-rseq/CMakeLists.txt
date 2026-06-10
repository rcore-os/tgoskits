cmake_minimum_required(VERSION 3.20)
project(test-rseq C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(test-rseq "x86_64" "test-rseq skipped on unsupported architecture" src/main.c)
target_include_directories(test-rseq PRIVATE src)
target_compile_options(test-rseq PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-rseq RUNTIME DESTINATION usr/bin/starry-test-suit)
