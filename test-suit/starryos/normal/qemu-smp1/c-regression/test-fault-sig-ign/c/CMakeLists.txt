cmake_minimum_required(VERSION 3.20)
project(test-fault-sig-ign C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-fault-sig-ign src/main.c)
target_include_directories(test-fault-sig-ign PRIVATE src)
target_compile_options(test-fault-sig-ign PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-fault-sig-ign RUNTIME DESTINATION usr/bin)
