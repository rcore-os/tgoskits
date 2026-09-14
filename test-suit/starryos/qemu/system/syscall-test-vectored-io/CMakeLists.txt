cmake_minimum_required(VERSION 3.20)

project(test-vectored-io C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-special-fd-write-precedence src/special_fd_write_precedence.c)
target_compile_options(test-special-fd-write-precedence PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-special-fd-write-precedence RUNTIME DESTINATION usr/bin/starry-test-suit)
