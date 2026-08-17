cmake_minimum_required(VERSION 3.20)

project(bugfix-xattrat-path-resolution C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-xattrat-path-resolution src/main.c)
target_compile_options(
    bugfix-xattrat-path-resolution
    PRIVATE -Wall -Wextra -Werror -U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=0
)
target_link_options(bugfix-xattrat-path-resolution PRIVATE -static)

install(
    TARGETS bugfix-xattrat-path-resolution
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
