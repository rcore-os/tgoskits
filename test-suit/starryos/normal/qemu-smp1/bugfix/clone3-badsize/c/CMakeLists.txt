cmake_minimum_required(VERSION 3.20)
project(clone3-badsize C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(clone3-badsize src/main.c)
target_compile_options(clone3-badsize PRIVATE -Wall -Wextra -Werror)

install(TARGETS clone3-badsize RUNTIME DESTINATION usr/bin)