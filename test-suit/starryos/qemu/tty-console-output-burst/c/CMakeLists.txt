cmake_minimum_required(VERSION 3.20)
project(tty-output-burst C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(tty-output-burst src/main.c)
target_compile_options(tty-output-burst PRIVATE -Wall -Wextra -Werror)

install(TARGETS tty-output-burst RUNTIME DESTINATION usr/bin)
