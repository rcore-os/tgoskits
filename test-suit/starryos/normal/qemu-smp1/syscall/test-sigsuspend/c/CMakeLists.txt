cmake_minimum_required(VERSION 3.20)
project(test-sigsuspend C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-sigsuspend src/main.c)
target_compile_options(test-sigsuspend PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-sigsuspend RUNTIME DESTINATION usr/bin)
