cmake_minimum_required(VERSION 3.20)
project(usercopy-fastpath-cow-write C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(usercopy-fastpath-cow-write src/main.c)
target_compile_options(usercopy-fastpath-cow-write PRIVATE -Wall -Wextra -Werror)

install(TARGETS usercopy-fastpath-cow-write RUNTIME DESTINATION usr/bin/starry-test-suit)
