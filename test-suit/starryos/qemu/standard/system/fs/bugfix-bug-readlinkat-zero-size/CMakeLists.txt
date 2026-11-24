cmake_minimum_required(VERSION 3.20)

project(bug-readlinkat-zero-size C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-readlinkat-zero-size src/main.c)
target_compile_options(bug-readlinkat-zero-size PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-readlinkat-zero-size RUNTIME DESTINATION usr/bin/starry-test-suit)
