cmake_minimum_required(VERSION 3.20)

project(bugfix-netlink-uevent-reuseaddr C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-netlink-uevent-reuseaddr src/main.c)
target_compile_options(bugfix-netlink-uevent-reuseaddr PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-netlink-uevent-reuseaddr RUNTIME DESTINATION usr/bin/starry-test-suit)
