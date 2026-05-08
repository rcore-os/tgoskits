cmake_minimum_required(VERSION 3.20)
project(bug-mmap-fd-zero-anon C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-mmap-fd-zero-anon src/main.c)
target_compile_options(bug-mmap-fd-zero-anon PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-mmap-fd-zero-anon RUNTIME DESTINATION usr/bin)
