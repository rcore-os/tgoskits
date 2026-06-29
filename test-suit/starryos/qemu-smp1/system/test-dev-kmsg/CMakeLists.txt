cmake_minimum_required(VERSION 3.20)

project(test_dev_kmsg C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-dev-kmsg src/main.c)
target_compile_options(test-dev-kmsg PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-dev-kmsg RUNTIME DESTINATION usr/bin/starry-test-suit)
