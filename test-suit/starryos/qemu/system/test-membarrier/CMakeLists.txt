cmake_minimum_required(VERSION 3.20)
project(test-membarrier C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-membarrier src/main.c)
target_include_directories(test-membarrier PRIVATE src)
target_compile_options(test-membarrier PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-membarrier RUNTIME DESTINATION usr/bin/starry-test-suit)
