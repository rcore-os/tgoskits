cmake_minimum_required(VERSION 3.20)
project(test-wait4-signal-interrupt C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-wait4-signal-interrupt src/main.c)
target_include_directories(test-wait4-signal-interrupt PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../../common)
target_compile_options(test-wait4-signal-interrupt PRIVATE -Wall -Wextra -Werror)
target_link_options(test-wait4-signal-interrupt PRIVATE -static)

install(TARGETS test-wait4-signal-interrupt RUNTIME DESTINATION usr/bin/starry-test-suit)
