cmake_minimum_required(VERSION 3.20)
project(test-user-backtrace C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-user-backtrace src/main.c)
target_compile_options(test-user-backtrace PRIVATE
    -Wall
    -Wextra
    -Werror
    -O0
    -fno-omit-frame-pointer
    -fno-optimize-sibling-calls
)

install(TARGETS test-user-backtrace RUNTIME DESTINATION usr/bin)
