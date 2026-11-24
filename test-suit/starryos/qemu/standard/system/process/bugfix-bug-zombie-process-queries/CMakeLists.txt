cmake_minimum_required(VERSION 3.20)
project(bug-zombie-process-queries C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-zombie-process-queries src/main.c)
target_compile_options(bug-zombie-process-queries PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-zombie-process-queries RUNTIME DESTINATION usr/bin/starry-test-suit)
