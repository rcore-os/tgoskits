cmake_minimum_required(VERSION 3.20)
project(test-unix-cmsg-byte-marks C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-unix-cmsg-byte-marks src/main.c)
target_compile_options(test-unix-cmsg-byte-marks PRIVATE -Wall -Wextra)

install(TARGETS test-unix-cmsg-byte-marks RUNTIME DESTINATION usr/bin/starry-test-suit)
