cmake_minimum_required(VERSION 3.20)
project(test-sigreturn C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-sigreturn src/main.c)
target_compile_options(test-sigreturn PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-sigreturn RUNTIME DESTINATION usr/bin/starry-test-suit)
