cmake_minimum_required(VERSION 3.20)
project(test-getgroups-abi-width C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-getgroups-abi-width src/main.c)
target_compile_options(test-getgroups-abi-width PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-getgroups-abi-width RUNTIME DESTINATION usr/bin/starry-test-suit)
