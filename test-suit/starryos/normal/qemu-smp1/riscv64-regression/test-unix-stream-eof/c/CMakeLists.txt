cmake_minimum_required(VERSION 3.20)
project(test-unix-stream-eof C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-unix-stream-eof src/main.c)
target_compile_options(test-unix-stream-eof PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-unix-stream-eof RUNTIME DESTINATION usr/bin)
