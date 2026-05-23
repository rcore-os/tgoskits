cmake_minimum_required(VERSION 3.20)

project(test-epoll-network C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-epoll-network src/main.c)
target_compile_options(test-epoll-network PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-epoll-network RUNTIME DESTINATION usr/bin/starry-test-suit)
