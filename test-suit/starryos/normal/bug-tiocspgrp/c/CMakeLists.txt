cmake_minimum_required(VERSION 3.20)

project(bug_tiocspgrp C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug_tiocspgrp src/main.c)
target_compile_options(bug_tiocspgrp PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug_tiocspgrp RUNTIME DESTINATION usr/bin)
