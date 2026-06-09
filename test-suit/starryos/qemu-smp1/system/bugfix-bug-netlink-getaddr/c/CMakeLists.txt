cmake_minimum_required(VERSION 3.20)

project(bug-netlink-getaddr C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-netlink-getaddr src/main.c)
target_compile_options(bug-netlink-getaddr PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-netlink-getaddr RUNTIME DESTINATION usr/bin/starry-test-suit)
