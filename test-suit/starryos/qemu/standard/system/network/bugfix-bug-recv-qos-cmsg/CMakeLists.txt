cmake_minimum_required(VERSION 3.20)

project(bug-recv-qos-cmsg C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-recv-qos-cmsg src/main.c)
target_compile_options(bug-recv-qos-cmsg PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-recv-qos-cmsg RUNTIME DESTINATION usr/bin/starry-test-suit)
