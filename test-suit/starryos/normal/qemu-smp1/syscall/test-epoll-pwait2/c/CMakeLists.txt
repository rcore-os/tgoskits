cmake_minimum_required(VERSION 3.20)
project(test-epoll-pwait2 C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-epoll-pwait2 src/main.c)
target_include_directories(test-epoll-pwait2 PRIVATE src)
target_compile_options(test-epoll-pwait2 PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-epoll-pwait2 RUNTIME DESTINATION usr/bin/starry-test-suit)
