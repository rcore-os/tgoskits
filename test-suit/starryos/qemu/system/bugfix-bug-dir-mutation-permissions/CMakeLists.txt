cmake_minimum_required(VERSION 3.20)
project(bug-dir-mutation-permissions C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-dir-mutation-permissions src/main.c)
target_compile_options(bug-dir-mutation-permissions PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-dir-mutation-permissions RUNTIME DESTINATION usr/bin/starry-test-suit)
