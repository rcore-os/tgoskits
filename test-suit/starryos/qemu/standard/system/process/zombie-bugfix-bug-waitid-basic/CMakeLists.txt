cmake_minimum_required(VERSION 3.20)
project(bug-waitid-basic C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-waitid-basic src/main.c)
target_compile_options(bug-waitid-basic PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-waitid-basic RUNTIME DESTINATION usr/bin/starry-test-suit)
