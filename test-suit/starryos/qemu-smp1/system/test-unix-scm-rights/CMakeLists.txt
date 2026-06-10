cmake_minimum_required(VERSION 3.20)
project(test-unix-scm-rights C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-unix-scm-rights src/main.c)
target_compile_options(test-unix-scm-rights PRIVATE -Wall -Wextra)

install(TARGETS test-unix-scm-rights RUNTIME DESTINATION usr/bin/starry-test-suit)
