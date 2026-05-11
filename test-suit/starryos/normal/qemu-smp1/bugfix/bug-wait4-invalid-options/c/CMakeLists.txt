cmake_minimum_required(VERSION 3.20)
project(bug-wait4-invalid-options C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-wait4-invalid-options src/main.c)
target_compile_options(bug-wait4-invalid-options PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-wait4-invalid-options RUNTIME DESTINATION usr/bin)
