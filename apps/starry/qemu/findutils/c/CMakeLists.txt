cmake_minimum_required(VERSION 3.20)

project(findutils-test C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(findutils-test src/main.c)
target_compile_options(findutils-test PRIVATE -Wall -Wextra -Werror)

install(TARGETS findutils-test RUNTIME DESTINATION usr/bin)
