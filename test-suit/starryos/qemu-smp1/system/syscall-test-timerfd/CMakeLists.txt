cmake_minimum_required(VERSION 3.20)
project(test-timerfd C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(test-timerfd src/main.c)
target_include_directories(test-timerfd PRIVATE src)
target_compile_options(test-timerfd PRIVATE -Wall -Wextra -Werror)
# Build this stricter probe, but keep it out of the grouped CI pass condition
# until timerfd's multi-reader single-consumer semantics are complete.
install(TARGETS test-timerfd RUNTIME DESTINATION usr/bin/starry-known-fail)
