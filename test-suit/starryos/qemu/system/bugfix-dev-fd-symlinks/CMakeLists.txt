cmake_minimum_required(VERSION 3.20)
project(bugfix-dev-fd-symlinks C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-dev-fd-symlinks src/main.c)
target_compile_options(bugfix-dev-fd-symlinks PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-dev-fd-symlinks RUNTIME DESTINATION usr/bin/starry-test-suit)
