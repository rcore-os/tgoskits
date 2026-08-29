cmake_minimum_required(VERSION 3.20)
project(test-ptrace-x86-xstate C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-ptrace-x86-xstate src/main.c)
target_compile_options(test-ptrace-x86-xstate PRIVATE -Wall -Wextra -Werror)
if(CMAKE_C_COMPILER_TARGET MATCHES "x86_64")
    target_compile_options(test-ptrace-x86-xstate PRIVATE -mavx)
endif()

install(TARGETS test-ptrace-x86-xstate RUNTIME DESTINATION usr/bin/starry-test-suit)
