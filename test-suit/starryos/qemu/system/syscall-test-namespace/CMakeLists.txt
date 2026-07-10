cmake_minimum_required(VERSION 3.20)
project(test-namespace C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-namespace src/main.c)
target_include_directories(test-namespace PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../common)
target_compile_options(test-namespace PRIVATE -Wall -Wextra -Werror)
target_link_options(test-namespace PRIVATE -static)

install(TARGETS test-namespace RUNTIME DESTINATION usr/bin/starry-test-suit)
