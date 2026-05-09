cmake_minimum_required(VERSION 3.20)
project(test-mt-execve C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-mt-execve src/main.c)
target_include_directories(test-mt-execve PRIVATE src)
target_compile_options(test-mt-execve PRIVATE -Wall -Wextra -pthread)
target_link_options(test-mt-execve PRIVATE -pthread)

install(TARGETS test-mt-execve RUNTIME DESTINATION usr/bin)
