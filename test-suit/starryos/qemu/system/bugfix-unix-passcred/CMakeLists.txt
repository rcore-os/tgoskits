cmake_minimum_required(VERSION 3.20)

project(bugfix-unix-passcred C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-unix-passcred src/main.c)
target_compile_options(bugfix-unix-passcred PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-unix-passcred RUNTIME DESTINATION usr/bin/starry-test-suit)
