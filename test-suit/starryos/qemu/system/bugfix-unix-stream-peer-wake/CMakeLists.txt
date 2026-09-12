cmake_minimum_required(VERSION 3.20)
project(bugfix-unix-stream-peer-wake C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bugfix-unix-stream-peer-wake src/main.c)
target_include_directories(bugfix-unix-stream-peer-wake PRIVATE ../common)
target_compile_options(bugfix-unix-stream-peer-wake PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-unix-stream-peer-wake RUNTIME DESTINATION usr/bin/starry-test-suit)
