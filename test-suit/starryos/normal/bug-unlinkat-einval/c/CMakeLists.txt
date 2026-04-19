cmake_minimum_required(VERSION 3.20)

project(bug-unlinkat-einval C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-unlinkat-einval src/main.c)
target_compile_options(bug-unlinkat-einval PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-unlinkat-einval RUNTIME DESTINATION usr/bin)
