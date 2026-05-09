cmake_minimum_required(VERSION 3.20)

project(bug-faccessat2-validation C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-faccessat2-validation src/main.c)
target_compile_options(bug-faccessat2-validation PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-faccessat2-validation RUNTIME DESTINATION usr/bin)
