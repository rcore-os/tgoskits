cmake_minimum_required(VERSION 3.20)
project(test-ptrace-exec-stop C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-ptrace-exec-stop src/main.c)
target_compile_options(test-ptrace-exec-stop PRIVATE
    -Wall
    -Wextra
    -Werror
)

install(TARGETS test-ptrace-exec-stop RUNTIME DESTINATION usr/bin)
