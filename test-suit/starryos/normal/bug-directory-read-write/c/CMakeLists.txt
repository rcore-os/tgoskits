cmake_minimum_required(VERSION 3.20)

project(bug_directory_read_write C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug_directory_read_write src/main.c)
target_compile_options(bug_directory_read_write PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug_directory_read_write RUNTIME DESTINATION usr/bin)
