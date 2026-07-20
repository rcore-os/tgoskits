cmake_minimum_required(VERSION 3.20)
project(test-dontwait C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-dontwait src/main.c)
target_compile_options(test-dontwait PRIVATE -Wall -Wextra)

install(TARGETS test-dontwait RUNTIME DESTINATION usr/bin/starry-test-suit)
