cmake_minimum_required(VERSION 3.20)
project(test-epoll C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-epoll src/main.c)
target_include_directories(test-epoll PRIVATE src)
target_compile_options(test-epoll PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-epoll RUNTIME DESTINATION usr/bin/starry-test-suit)
