cmake_minimum_required(VERSION 3.20)

project(bug-preadv2-offset-neg1 C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-preadv2-offset-neg1 src/main.c)
target_compile_options(bug-preadv2-offset-neg1 PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-preadv2-offset-neg1 RUNTIME DESTINATION usr/bin)
