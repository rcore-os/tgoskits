cmake_minimum_required(VERSION 3.20)
project(test-overlayfs C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-overlayfs src/main.c)
target_include_directories(test-overlayfs PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../common)
target_compile_options(test-overlayfs PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-overlayfs RUNTIME DESTINATION usr/bin/starry-test-suit)
