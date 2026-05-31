cmake_minimum_required(VERSION 3.20)
project(test-ptrace-x86-breakpoint-reinsert C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-ptrace-x86-breakpoint-reinsert src/main.c)
target_compile_options(test-ptrace-x86-breakpoint-reinsert PRIVATE -Wall -Wextra -Werror -O0 -g)

install(TARGETS test-ptrace-x86-breakpoint-reinsert RUNTIME DESTINATION usr/bin)
