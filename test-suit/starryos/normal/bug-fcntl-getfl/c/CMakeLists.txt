cmake_minimum_required(VERSION 3.20)

project(bug_fcntl_getfl C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug_fcntl_getfl src/main.c)
target_compile_options(bug_fcntl_getfl PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug_fcntl_getfl RUNTIME DESTINATION usr/bin)
