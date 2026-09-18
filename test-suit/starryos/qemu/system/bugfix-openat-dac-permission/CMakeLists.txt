cmake_minimum_required(VERSION 3.20)
project(bugfix-openat-dac-permission C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bugfix-openat-dac-permission src/main.c)
target_include_directories(bugfix-openat-dac-permission PRIVATE ../common)
target_compile_options(bugfix-openat-dac-permission PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-openat-dac-permission RUNTIME DESTINATION usr/bin/starry-test-suit)
