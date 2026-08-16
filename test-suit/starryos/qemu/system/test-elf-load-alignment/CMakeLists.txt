cmake_minimum_required(VERSION 3.20)
project(test-elf-load-alignment C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-elf-load-alignment src/main.c)
target_compile_options(test-elf-load-alignment PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-elf-load-alignment RUNTIME DESTINATION usr/bin/starry-test-suit)
