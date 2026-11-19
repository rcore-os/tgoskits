cmake_minimum_required(VERSION 3.20)

project(bugfix-tcp-half-close-shutdown C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-tcp-half-close-shutdown src/main.c)
target_compile_options(bugfix-tcp-half-close-shutdown PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-tcp-half-close-shutdown RUNTIME DESTINATION usr/bin/starry-test-suit)
