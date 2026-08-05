cmake_minimum_required(VERSION 3.20)

project(bug-mount-bind-file C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-mount-bind-file src/main.c)
target_compile_options(bug-mount-bind-file PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-mount-bind-file RUNTIME DESTINATION usr/bin/starry-test-suit)
