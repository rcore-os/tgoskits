cmake_minimum_required(VERSION 3.20)

project(bug-mkdir-empty-path C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-mkdir-empty-path src/main.c)
target_compile_options(bug-mkdir-empty-path PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-mkdir-empty-path RUNTIME DESTINATION usr/bin)
