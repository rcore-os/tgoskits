cmake_minimum_required(VERSION 3.20)

project(flock-last-close C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(flock-last-close src/main.c)
target_compile_options(flock-last-close PRIVATE -Wall -Wextra -Werror)

install(TARGETS flock-last-close RUNTIME DESTINATION usr/bin/starry-test-suit)
