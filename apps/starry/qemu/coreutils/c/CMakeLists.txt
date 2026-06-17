cmake_minimum_required(VERSION 3.20)
project(coreutils-test C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(coreutils-test src/main.c)
target_compile_options(coreutils-test PRIVATE -Wall -Wextra -Werror)

install(TARGETS coreutils-test RUNTIME DESTINATION usr/bin)
