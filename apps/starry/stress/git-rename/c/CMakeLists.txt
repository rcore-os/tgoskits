cmake_minimum_required(VERSION 3.20)
project(rename-probe C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(rename-probe src/main.c)
target_compile_options(rename-probe PRIVATE -Wall -Wextra -Werror)

install(TARGETS rename-probe RUNTIME DESTINATION usr/bin)
