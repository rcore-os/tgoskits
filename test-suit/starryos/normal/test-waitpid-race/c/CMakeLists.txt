cmake_minimum_required(VERSION 3.20)
project(test-waitpid-race C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-waitpid-race src/main.c)
target_compile_options(test-waitpid-race PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-waitpid-race RUNTIME DESTINATION usr/bin)
