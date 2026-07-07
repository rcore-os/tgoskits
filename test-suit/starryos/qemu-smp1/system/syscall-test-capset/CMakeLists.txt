cmake_minimum_required(VERSION 3.20)
project(test-capset C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-capset src/main.c)
target_include_directories(test-capset PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../../common)
target_compile_options(test-capset PRIVATE -Wall -Wextra -Werror)
target_link_options(test-capset PRIVATE -static)

install(TARGETS test-capset RUNTIME DESTINATION usr/bin/starry-test-suit)
