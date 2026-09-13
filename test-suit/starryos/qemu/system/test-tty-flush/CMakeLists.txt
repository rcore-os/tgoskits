cmake_minimum_required(VERSION 3.20)
project(test-tty-flush C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-tty-flush src/main.c)
target_compile_options(test-tty-flush PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-tty-flush RUNTIME DESTINATION usr/bin/starry-test-suit)
