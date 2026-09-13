cmake_minimum_required(VERSION 3.20)
project(bugfix-systemd-catalog-io C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-systemd-catalog-io src/main.c)
target_compile_options(
    bugfix-systemd-catalog-io
    PRIVATE -Wall -Wextra -Werror
)
install(
    TARGETS bugfix-systemd-catalog-io
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
