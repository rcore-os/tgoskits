cmake_minimum_required(VERSION 3.20)
project(test-proc-environ C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-proc-environ src/main.c)
target_compile_options(test-proc-environ PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-proc-environ RUNTIME DESTINATION usr/bin/starry-test-suit)
