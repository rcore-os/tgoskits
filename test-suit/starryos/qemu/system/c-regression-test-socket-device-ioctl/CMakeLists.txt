cmake_minimum_required(VERSION 3.20)

project(socket-device-ioctl C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(socket-device-ioctl src/main.c)
target_compile_options(socket-device-ioctl PRIVATE -Wall -Wextra -Werror)

install(TARGETS socket-device-ioctl RUNTIME DESTINATION usr/bin/starry-test-suit)
