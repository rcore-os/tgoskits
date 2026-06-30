cmake_minimum_required(VERSION 3.20)
project(bug-usercopy-cold-page C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-usercopy-cold-page src/main.c)
target_compile_options(bug-usercopy-cold-page PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-usercopy-cold-page RUNTIME DESTINATION usr/bin/starry-test-suit)
