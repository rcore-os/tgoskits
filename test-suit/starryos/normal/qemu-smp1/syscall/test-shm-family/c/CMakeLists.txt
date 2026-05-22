cmake_minimum_required(VERSION 3.20)
project(test-shm-family C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-shm-family src/main.c)
target_include_directories(test-shm-family PRIVATE src)
target_compile_options(test-shm-family PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-shm-family RUNTIME DESTINATION usr/bin/starry-test-suit)
