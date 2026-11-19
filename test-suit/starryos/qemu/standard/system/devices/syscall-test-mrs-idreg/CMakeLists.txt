cmake_minimum_required(VERSION 3.20)
project(test-mrs-idreg C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-mrs-idreg src/main.c)
target_compile_options(test-mrs-idreg PRIVATE -Wall -Wextra)
target_link_options(test-mrs-idreg PRIVATE -static)

install(TARGETS test-mrs-idreg RUNTIME DESTINATION usr/bin/starry-test-suit)
