cmake_minimum_required(VERSION 3.20)

project(grep-sed-awk-test C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(grep-sed-awk-test src/main.c)
target_compile_options(grep-sed-awk-test PRIVATE -Wall -Wextra -Werror)

install(TARGETS grep-sed-awk-test RUNTIME DESTINATION usr/bin)
