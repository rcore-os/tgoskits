cmake_minimum_required(VERSION 3.20)
project(test-rename-content C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-rename-content src/main.c)
target_compile_options(test-rename-content PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-rename-content RUNTIME DESTINATION usr/bin)
