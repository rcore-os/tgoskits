cmake_minimum_required(VERSION 3.20)
project(test-max-ns-entries C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-max-ns-entries src/main.c)
target_compile_options(test-max-ns-entries PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-max-ns-entries RUNTIME DESTINATION usr/bin/starry-test-suit)
