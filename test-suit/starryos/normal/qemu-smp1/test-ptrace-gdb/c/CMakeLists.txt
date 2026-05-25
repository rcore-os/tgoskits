cmake_minimum_required(VERSION 3.20)
project(test-ptrace-gdb C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-ptrace-gdb src/main.c)
target_compile_options(test-ptrace-gdb PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-ptrace-gdb RUNTIME DESTINATION usr/bin)
