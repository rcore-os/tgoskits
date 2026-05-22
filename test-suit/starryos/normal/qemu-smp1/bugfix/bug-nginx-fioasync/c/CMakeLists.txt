cmake_minimum_required(VERSION 3.20)
project(bug-nginx-fioasync C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-nginx-fioasync src/main.c)
target_compile_options(bug-nginx-fioasync PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-nginx-fioasync RUNTIME DESTINATION usr/bin)
