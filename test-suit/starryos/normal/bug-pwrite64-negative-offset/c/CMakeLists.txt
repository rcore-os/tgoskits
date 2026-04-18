cmake_minimum_required(VERSION 3.20)

project(bug_pwrite64_negative_offset C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug_pwrite64_negative_offset src/main.c)
target_compile_options(bug_pwrite64_negative_offset PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug_pwrite64_negative_offset RUNTIME DESTINATION usr/bin)
