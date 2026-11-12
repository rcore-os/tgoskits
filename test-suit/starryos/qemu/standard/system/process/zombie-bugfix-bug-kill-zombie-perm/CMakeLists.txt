cmake_minimum_required(VERSION 3.20)

project(bug-kill-zombie-perm C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-kill-zombie-perm src/main.c)
target_compile_options(bug-kill-zombie-perm PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-kill-zombie-perm RUNTIME DESTINATION usr/bin/starry-test-suit)
