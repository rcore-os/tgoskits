cmake_minimum_required(VERSION 3.20)

project(bugfix-proc-sys-kernel-hostname C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-proc-sys-kernel-hostname src/main.c)
target_compile_options(bugfix-proc-sys-kernel-hostname PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-proc-sys-kernel-hostname RUNTIME DESTINATION usr/bin/starry-test-suit)
