cmake_minimum_required(VERSION 3.20)
project(test-capget C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-capget src/main.c)
target_include_directories(test-capget PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../../common)
target_compile_options(test-capget PRIVATE -Wall -Wextra -Werror)
target_link_options(test-capget PRIVATE -static)

install(TARGETS test-capget RUNTIME DESTINATION usr/bin/starry-test-suit)
