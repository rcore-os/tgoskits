cmake_minimum_required(VERSION 3.20)

project(bug-tcp-udp-bind-same-port C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-tcp-udp-bind-same-port src/main.c)
target_compile_options(bug-tcp-udp-bind-same-port PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-tcp-udp-bind-same-port RUNTIME DESTINATION usr/bin/starry-test-suit)
