cmake_minimum_required(VERSION 3.20)
project(test-ptrace-traceme-stop C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-ptrace-traceme-stop src/main.c)
target_compile_options(test-ptrace-traceme-stop PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-ptrace-traceme-stop RUNTIME DESTINATION usr/bin)
