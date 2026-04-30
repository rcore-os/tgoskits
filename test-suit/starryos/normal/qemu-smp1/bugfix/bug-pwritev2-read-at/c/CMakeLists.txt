cmake_minimum_required(VERSION 3.20)
project(bug-pwritev2-read-at C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-pwritev2-read-at src/main.c)
target_compile_options(bug-pwritev2-read-at PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-pwritev2-read-at RUNTIME DESTINATION usr/bin)
