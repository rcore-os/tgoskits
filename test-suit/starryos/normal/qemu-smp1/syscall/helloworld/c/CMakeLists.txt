cmake_minimum_required(VERSION 3.20)

project(helloworld C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(helloworld src/main.c)
target_compile_options(helloworld PRIVATE -Wall -Wextra -Werror)

install(TARGETS helloworld RUNTIME DESTINATION usr/bin)
