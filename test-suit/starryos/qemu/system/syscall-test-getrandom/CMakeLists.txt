cmake_minimum_required(VERSION 3.20)
project(syscall-test-getrandom C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(syscall-test-getrandom src/main.c)
target_include_directories(syscall-test-getrandom PRIVATE ../common)
target_compile_options(syscall-test-getrandom PRIVATE -Wall -Wextra -Werror)

install(TARGETS syscall-test-getrandom RUNTIME DESTINATION usr/bin/starry-test-suit)
