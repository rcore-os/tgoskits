cmake_minimum_required(VERSION 3.20)
project(test_sa_restart C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test_sa_restart src/main.c)
target_include_directories(test_sa_restart PRIVATE src)
target_compile_options(test_sa_restart PRIVATE -Wall -Wextra)

install(TARGETS test_sa_restart RUNTIME DESTINATION usr/bin)
