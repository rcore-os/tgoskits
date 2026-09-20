cmake_minimum_required(VERSION 3.20)
project(bugfix-setns-requires-cap-sys-admin C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bugfix-setns-requires-cap-sys-admin src/main.c)
target_include_directories(bugfix-setns-requires-cap-sys-admin PRIVATE ../common)
target_compile_options(bugfix-setns-requires-cap-sys-admin PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-setns-requires-cap-sys-admin RUNTIME DESTINATION usr/bin/starry-test-suit)
