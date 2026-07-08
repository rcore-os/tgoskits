cmake_minimum_required(VERSION 3.20)
project(bug-tcp-nonblocking-connect-so-error C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-tcp-nonblocking-connect-so-error src/main.c)
target_compile_options(bug-tcp-nonblocking-connect-so-error PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-tcp-nonblocking-connect-so-error RUNTIME DESTINATION usr/bin/starry-test-suit)
