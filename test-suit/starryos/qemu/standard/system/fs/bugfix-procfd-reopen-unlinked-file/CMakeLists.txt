cmake_minimum_required(VERSION 3.20)
project(bugfix-procfd-reopen-unlinked-file C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-procfd-reopen-unlinked-file src/main.c)
target_compile_options(
    bugfix-procfd-reopen-unlinked-file
    PRIVATE -Wall -Wextra -Werror
)

install(
    TARGETS bugfix-procfd-reopen-unlinked-file
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
