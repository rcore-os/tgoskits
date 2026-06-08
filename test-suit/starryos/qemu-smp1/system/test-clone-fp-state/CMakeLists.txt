cmake_minimum_required(VERSION 3.20)
project(test-clone-fp-state C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-clone-fp-state src/main.c)
target_compile_options(test-clone-fp-state PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-clone-fp-state RUNTIME DESTINATION usr/bin/starry-test-suit)
