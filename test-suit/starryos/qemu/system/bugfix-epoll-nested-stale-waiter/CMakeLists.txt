cmake_minimum_required(VERSION 3.20)
project(bugfix-epoll-nested-stale-waiter C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bugfix-epoll-nested-stale-waiter src/main.c)
target_include_directories(bugfix-epoll-nested-stale-waiter PRIVATE ../common)
target_compile_options(bugfix-epoll-nested-stale-waiter PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-epoll-nested-stale-waiter RUNTIME DESTINATION usr/bin/starry-test-suit)
