cmake_minimum_required(VERSION 3.20)
project(test-msgget C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-msgget src/main.c)
target_include_directories(test-msgget PRIVATE src)
target_compile_options(test-msgget PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-msgget RUNTIME DESTINATION usr/bin/starry-test-suit)
