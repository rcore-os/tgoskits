cmake_minimum_required(VERSION 3.20)
project(bugfix-ramfs-mount C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-ramfs-mount src/main.c)
target_compile_options(bugfix-ramfs-mount PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-ramfs-mount RUNTIME DESTINATION usr/bin/starry-test-suit)
