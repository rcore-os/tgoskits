cmake_minimum_required(VERSION 3.20)
project(test-unix-bind-chown C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-unix-bind-chown src/main.c)
target_compile_options(test-unix-bind-chown PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-unix-bind-chown RUNTIME DESTINATION usr/bin)
