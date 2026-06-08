cmake_minimum_required(VERSION 3.20)
project(test-ptrace-wait-wall C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-ptrace-wait-wall src/main.c)
target_compile_options(test-ptrace-wait-wall PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-ptrace-wait-wall RUNTIME DESTINATION usr/bin)
