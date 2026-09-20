cmake_minimum_required(VERSION 3.20)
project(test-setgroups-abi-width C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-setgroups-abi-width src/main.c)
target_compile_options(test-setgroups-abi-width PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-setgroups-abi-width RUNTIME DESTINATION usr/bin/starry-test-suit)
