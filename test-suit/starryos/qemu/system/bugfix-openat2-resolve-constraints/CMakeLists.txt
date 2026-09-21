cmake_minimum_required(VERSION 3.20)
project(bugfix-openat2-resolve-constraints C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-openat2-resolve-constraints src/main.c)
target_compile_options(bugfix-openat2-resolve-constraints PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-openat2-resolve-constraints RUNTIME DESTINATION usr/bin/starry-test-suit)
