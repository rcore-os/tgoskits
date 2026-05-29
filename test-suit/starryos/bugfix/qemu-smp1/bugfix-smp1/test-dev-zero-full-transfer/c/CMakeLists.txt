cmake_minimum_required(VERSION 3.20)

project(test_dev_zero_full_transfer C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-dev-zero-full-transfer src/main.c)
target_compile_options(test-dev-zero-full-transfer PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-dev-zero-full-transfer RUNTIME DESTINATION usr/bin)
