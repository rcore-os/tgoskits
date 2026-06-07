cmake_minimum_required(VERSION 3.20)
project(test-mlock2 C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-mlock2 src/main.c)
target_compile_options(test-mlock2 PRIVATE -Wall -Wextra)
target_link_options(test-mlock2 PRIVATE -static)

install(TARGETS test-mlock2 RUNTIME DESTINATION usr/bin/starry-test-suit)
