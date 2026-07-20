cmake_minimum_required(VERSION 3.20)
project(bug-tcp-keepalive-options C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-tcp-keepalive-options src/main.c)
target_compile_options(bug-tcp-keepalive-options PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-tcp-keepalive-options RUNTIME DESTINATION usr/bin/starry-test-suit)
