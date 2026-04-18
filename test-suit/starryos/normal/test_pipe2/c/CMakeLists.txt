cmake_minimum_required(VERSION 3.20)

project(test_pipe2 C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test_pipe2 src/main.c)
target_compile_options(test_pipe2 PRIVATE -Wall -Wextra -Werror)

install(TARGETS test_pipe2 RUNTIME DESTINATION usr/bin)
