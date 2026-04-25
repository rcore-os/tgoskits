cmake_minimum_required(VERSION 3.20)

project(bug-preadv2-invalid-flags C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-preadv2-invalid-flags src/main.c)
target_compile_options(bug-preadv2-invalid-flags PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-preadv2-invalid-flags RUNTIME DESTINATION usr/bin)
