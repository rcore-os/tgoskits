cmake_minimum_required(VERSION 3.20)

project(bug-epoll-timeout C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-epoll-timeout src/main.c)
target_compile_options(bug-epoll-timeout PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-epoll-timeout RUNTIME DESTINATION usr/bin)
