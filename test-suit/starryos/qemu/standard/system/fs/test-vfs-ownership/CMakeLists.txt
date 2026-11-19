cmake_minimum_required(VERSION 3.10)
project(test-vfs-ownership C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_FLAGS "${CMAKE_C_FLAGS} -Os -static -Wall -Wextra")

add_executable(test-vfs-ownership src/main.c)
install(TARGETS test-vfs-ownership RUNTIME DESTINATION usr/bin/starry-test-suit)
