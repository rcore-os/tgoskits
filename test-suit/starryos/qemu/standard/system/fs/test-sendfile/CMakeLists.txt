cmake_minimum_required(VERSION 3.20)
project(test_sendfile C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

add_executable(test-sendfile src/main.c)

target_compile_options(test-sendfile PRIVATE -Wall -Wextra)

install(TARGETS test-sendfile RUNTIME DESTINATION usr/bin/starry-test-suit)