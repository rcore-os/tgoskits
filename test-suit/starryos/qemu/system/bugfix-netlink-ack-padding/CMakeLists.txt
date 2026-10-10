cmake_minimum_required(VERSION 3.20)

project(netlink-ack-padding C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(netlink-ack-padding src/main.c)
target_compile_options(netlink-ack-padding PRIVATE -Wall -Wextra -Werror)

install(TARGETS netlink-ack-padding RUNTIME DESTINATION usr/bin/starry-test-suit)
