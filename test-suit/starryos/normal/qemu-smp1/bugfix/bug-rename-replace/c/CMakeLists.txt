cmake_minimum_required(VERSION 3.20)

project(bug-rename-replace C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-rename-replace src/main.c)
target_compile_options(bug-rename-replace PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-rename-replace RUNTIME DESTINATION usr/bin)
