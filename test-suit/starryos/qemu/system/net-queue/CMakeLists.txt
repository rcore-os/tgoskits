cmake_minimum_required(VERSION 3.20)
project(net-queue C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(net-queue src/main.c)
target_compile_options(net-queue PRIVATE -Wall -Wextra -Werror)
install(TARGETS net-queue RUNTIME DESTINATION usr/bin/starry-test-suit)
