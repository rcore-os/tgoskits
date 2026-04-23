cmake_minimum_required(VERSION 3.20)
project(test-ipv6-socket C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-ipv6-socket src/main.c)
target_include_directories(test-ipv6-socket PRIVATE src)
target_compile_options(test-ipv6-socket PRIVATE -Wall -Wextra)

install(TARGETS test-ipv6-socket RUNTIME DESTINATION usr/bin)
