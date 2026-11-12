cmake_minimum_required(VERSION 3.20)
project(test-mmap-prot-write C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-mmap-prot-write src/main.c)
target_compile_options(test-mmap-prot-write PRIVATE -Wall -Wextra)

install(TARGETS test-mmap-prot-write RUNTIME DESTINATION usr/bin/starry-test-suit)
