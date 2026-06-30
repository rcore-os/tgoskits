cmake_minimum_required(VERSION 3.20)

project(bug-socket-qos-options C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-socket-qos-options src/main.c)
target_compile_options(bug-socket-qos-options PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-socket-qos-options RUNTIME DESTINATION usr/bin/starry-test-suit)
