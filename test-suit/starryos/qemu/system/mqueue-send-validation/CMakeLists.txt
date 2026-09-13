cmake_minimum_required(VERSION 3.20)
project(test-mqueue-send-validation C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-mqueue-send-validation src/main.c)
target_compile_options(test-mqueue-send-validation PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-mqueue-send-validation RUNTIME DESTINATION usr/bin/starry-test-suit)
