cmake_minimum_required(VERSION 3.20)

project(bug-vfork-sniff C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-vfork-sniff src/main.c)
target_compile_options(bug-vfork-sniff PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-vfork-sniff RUNTIME DESTINATION usr/bin)