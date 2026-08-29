cmake_minimum_required(VERSION 3.20)
project(test-x86-exec-fpu-reset C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-x86-exec-fpu-reset src/main.c)
target_compile_options(test-x86-exec-fpu-reset PRIVATE -Wall -Wextra)
install(TARGETS test-x86-exec-fpu-reset RUNTIME DESTINATION usr/bin/starry-test-suit)
