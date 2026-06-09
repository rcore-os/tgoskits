cmake_minimum_required(VERSION 3.20)
project(test-unix-msg-peek C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-unix-msg-peek src/main.c)
target_compile_options(test-unix-msg-peek PRIVATE -Wall -Wextra)

install(TARGETS test-unix-msg-peek RUNTIME DESTINATION usr/bin/starry-test-suit)
