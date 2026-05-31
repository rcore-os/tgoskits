cmake_minimum_required(VERSION 3.20)
project(test-ptrace-x86-singlestep C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-ptrace-x86-singlestep src/main.c)
target_compile_options(test-ptrace-x86-singlestep PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-ptrace-x86-singlestep RUNTIME DESTINATION usr/bin)
