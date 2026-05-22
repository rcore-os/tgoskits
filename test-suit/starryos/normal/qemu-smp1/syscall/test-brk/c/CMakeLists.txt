cmake_minimum_required(VERSION 3.20)
project(test-brk C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-brk src/main.c)
target_compile_options(test-brk PRIVATE -Wall -Wextra)
target_link_options(test-brk PRIVATE -static)

install(TARGETS test-brk RUNTIME DESTINATION usr/bin/starry-test-suit)