cmake_minimum_required(VERSION 3.20)
project(bug-tcp-concurrent-connect C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-tcp-concurrent-connect src/main.c)
target_compile_options(bug-tcp-concurrent-connect PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-tcp-concurrent-connect RUNTIME DESTINATION usr/bin)
