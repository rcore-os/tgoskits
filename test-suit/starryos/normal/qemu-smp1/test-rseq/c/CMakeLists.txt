cmake_minimum_required(VERSION 3.20)
project(test-rseq C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-rseq src/main.c)
target_include_directories(test-rseq PRIVATE src)
target_compile_options(test-rseq PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-rseq RUNTIME DESTINATION usr/bin)
