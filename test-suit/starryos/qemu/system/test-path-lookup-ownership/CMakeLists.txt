cmake_minimum_required(VERSION 3.20)
project(test-path-lookup-ownership C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-path-lookup-ownership src/main.c)
target_compile_options(test-path-lookup-ownership PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-path-lookup-ownership RUNTIME DESTINATION usr/bin/starry-test-suit)
