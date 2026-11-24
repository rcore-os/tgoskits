cmake_minimum_required(VERSION 3.20)
project(test-sio-set-ifflags C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-sio-set-ifflags src/main.c)
target_compile_options(test-sio-set-ifflags PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-sio-set-ifflags RUNTIME DESTINATION usr/bin/starry-test-suit)
