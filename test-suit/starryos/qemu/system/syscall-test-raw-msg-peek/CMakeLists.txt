cmake_minimum_required(VERSION 3.20)
project(test-raw-msg-peek C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-raw-msg-peek src/main.c)
target_include_directories(test-raw-msg-peek PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../../common)
target_compile_options(test-raw-msg-peek PRIVATE -Wall -Wextra -Werror)
target_link_options(test-raw-msg-peek PRIVATE -static)

install(TARGETS test-raw-msg-peek RUNTIME DESTINATION usr/bin/starry-test-suit)
