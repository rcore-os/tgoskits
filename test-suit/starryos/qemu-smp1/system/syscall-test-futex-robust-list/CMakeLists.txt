cmake_minimum_required(VERSION 3.20)
project(test-futex-robust-list C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-futex-robust-list src/main.c)
target_include_directories(test-futex-robust-list PRIVATE src)
target_compile_options(test-futex-robust-list PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-futex-robust-list PRIVATE pthread)

install(TARGETS test-futex-robust-list RUNTIME DESTINATION usr/bin/starry-test-suit)
