cmake_minimum_required(VERSION 3.20)
project(bugfix-statx-mount-root C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-statx-mount-root src/main.c)
target_compile_options(bugfix-statx-mount-root PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-statx-mount-root RUNTIME DESTINATION usr/bin/starry-test-suit)
