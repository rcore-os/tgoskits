cmake_minimum_required(VERSION 3.20)

project(bugfix-epoll-lt-fairness C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-epoll-lt-fairness src/main.c)
target_compile_options(bugfix-epoll-lt-fairness PRIVATE -Wall -Wextra -Werror)

install(
    TARGETS bugfix-epoll-lt-fairness
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
