cmake_minimum_required(VERSION 3.20)

project(bug_mremap_shared C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug_mremap_shared src/main.c)
target_compile_options(bug_mremap_shared PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug_mremap_shared RUNTIME DESTINATION usr/bin)
