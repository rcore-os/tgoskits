cmake_minimum_required(VERSION 3.20)
project(test-msgctl C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-msgctl src/main.c)
target_include_directories(test-msgctl PRIVATE src)
target_compile_options(test-msgctl PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-msgctl RUNTIME DESTINATION usr/bin/starry-test-suit)
