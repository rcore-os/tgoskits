cmake_minimum_required(VERSION 3.20)

project(bugfix-socket-timestamp C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-socket-timestamp src/main.c)
target_compile_options(bugfix-socket-timestamp PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-socket-timestamp RUNTIME DESTINATION usr/bin/starry-test-suit)
