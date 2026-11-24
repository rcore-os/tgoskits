cmake_minimum_required(VERSION 3.20)
project(bug-nginx-fionread-socket C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-nginx-fionread-socket src/main.c)
target_compile_options(bug-nginx-fionread-socket PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-nginx-fionread-socket RUNTIME DESTINATION usr/bin/starry-test-suit)
