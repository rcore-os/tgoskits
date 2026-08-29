cmake_minimum_required(VERSION 3.20)
project(test-x86-avx-sigreturn C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-x86-avx-sigreturn src/main.c)
target_compile_options(test-x86-avx-sigreturn PRIVATE -Wall -Wextra -Werror)
if(CMAKE_C_COMPILER_TARGET MATCHES "x86_64")
    target_compile_options(test-x86-avx-sigreturn PRIVATE -mavx)
endif()

install(TARGETS test-x86-avx-sigreturn RUNTIME DESTINATION usr/bin/starry-test-suit)
