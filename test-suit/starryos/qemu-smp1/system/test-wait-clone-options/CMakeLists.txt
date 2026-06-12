cmake_minimum_required(VERSION 3.20)
project(test-wait-clone-options C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-wait-clone-options src/main.c)
target_compile_options(test-wait-clone-options PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-wait-clone-options RUNTIME DESTINATION usr/bin/starry-test-suit)
