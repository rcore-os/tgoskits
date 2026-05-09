cmake_minimum_required(VERSION 3.20)

project(times_test C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(times_test src/main.c)
target_include_directories(times_test PRIVATE include)
target_compile_options(times_test PRIVATE -Wall -Wextra -Werror)

install(TARGETS times_test RUNTIME DESTINATION usr/bin)
