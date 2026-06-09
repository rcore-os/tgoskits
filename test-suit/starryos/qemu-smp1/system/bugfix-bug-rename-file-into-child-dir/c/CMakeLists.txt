cmake_minimum_required(VERSION 3.20)
project(bug-rename-file-into-child-dir C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-rename-file-into-child-dir src/main.c)
target_compile_options(bug-rename-file-into-child-dir PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-rename-file-into-child-dir RUNTIME DESTINATION usr/bin/starry-test-suit)
