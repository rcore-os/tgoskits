cmake_minimum_required(VERSION 3.20)

project(flock-in-flight C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(flock-in-flight src/main.c)
target_compile_options(flock-in-flight PRIVATE -Wall -Wextra -Werror)

install(TARGETS flock-in-flight RUNTIME DESTINATION usr/bin/starry-test-suit)
