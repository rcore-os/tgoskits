cmake_minimum_required(VERSION 3.20)
project(syscall-test-rlimit-as C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(syscall-test-rlimit-as src/main.c)
target_include_directories(syscall-test-rlimit-as PRIVATE ../common)
target_compile_options(syscall-test-rlimit-as PRIVATE -Wall -Wextra -Werror)

install(TARGETS syscall-test-rlimit-as RUNTIME DESTINATION usr/bin/starry-test-suit)
