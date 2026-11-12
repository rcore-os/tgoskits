cmake_minimum_required(VERSION 3.20)
project(test-calloc-mallocng C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-calloc-mallocng src/main.c)
target_compile_options(test-calloc-mallocng PRIVATE -Wall -Wextra -Werror)
target_link_options(test-calloc-mallocng PRIVATE -static)

install(TARGETS test-calloc-mallocng RUNTIME DESTINATION usr/bin/starry-test-suit)
