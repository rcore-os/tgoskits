cmake_minimum_required(VERSION 3.20)

project(test-vectored-io C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-vectored-io src/main.c)
target_compile_options(test-vectored-io PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-vectored-io RUNTIME DESTINATION usr/bin)
