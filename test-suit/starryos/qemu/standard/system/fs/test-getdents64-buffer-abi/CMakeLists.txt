cmake_minimum_required(VERSION 3.20)
project(test-getdents64-buffer-abi C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-getdents64-buffer-abi src/main.c)
target_compile_options(test-getdents64-buffer-abi PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-getdents64-buffer-abi RUNTIME DESTINATION usr/bin/starry-test-suit)
