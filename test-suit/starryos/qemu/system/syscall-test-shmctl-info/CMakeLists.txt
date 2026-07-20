cmake_minimum_required(VERSION 3.20)
project(test-shmctl-info C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-shmctl-info src/main.c)
target_include_directories(test-shmctl-info PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../common)
target_compile_options(test-shmctl-info PRIVATE -Wall -Wextra -Werror)
target_link_options(test-shmctl-info PRIVATE -static)

install(TARGETS test-shmctl-info RUNTIME DESTINATION usr/bin/starry-test-suit)
