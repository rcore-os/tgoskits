cmake_minimum_required(VERSION 3.20)

project(bug-writev-dir-ebadf C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-writev-dir-ebadf src/main.c)
target_compile_options(bug-writev-dir-ebadf PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-writev-dir-ebadf RUNTIME DESTINATION usr/bin)
