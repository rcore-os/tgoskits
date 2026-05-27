cmake_minimum_required(VERSION 3.20)
project(test-namespace C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-namespace src/main.c)
target_include_directories(test-namespace PRIVATE src)
target_compile_options(test-namespace PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-namespace RUNTIME DESTINATION usr/bin/starry-test-suit)
