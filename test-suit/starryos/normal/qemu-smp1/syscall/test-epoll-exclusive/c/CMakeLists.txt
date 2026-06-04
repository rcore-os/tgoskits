cmake_minimum_required(VERSION 3.20)
project(test-epoll-exclusive C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-epoll-exclusive src/main.c)
target_include_directories(test-epoll-exclusive PRIVATE ../../common)
target_compile_options(test-epoll-exclusive PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-epoll-exclusive RUNTIME DESTINATION usr/bin/starry-test-suit)
