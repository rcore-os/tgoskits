cmake_minimum_required(VERSION 3.20)

project(bugfix-proc-sys-kernel-random-boot-id C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-proc-sys-kernel-random-boot-id src/main.c)
target_compile_options(
    bugfix-proc-sys-kernel-random-boot-id
    PRIVATE -Wall -Wextra -Werror
)
target_link_options(bugfix-proc-sys-kernel-random-boot-id PRIVATE -static)

install(
    TARGETS bugfix-proc-sys-kernel-random-boot-id
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
