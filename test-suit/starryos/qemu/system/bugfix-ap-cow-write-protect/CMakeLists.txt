cmake_minimum_required(VERSION 3.20)
project(ap-cow-write-protect C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(ap-cow-write-protect src/main.c)
target_compile_options(ap-cow-write-protect PRIVATE -Wall -Wextra -Werror)

install(TARGETS ap-cow-write-protect RUNTIME DESTINATION usr/bin/starry-test-suit)
