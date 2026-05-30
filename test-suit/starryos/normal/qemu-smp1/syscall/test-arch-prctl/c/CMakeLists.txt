cmake_minimum_required(VERSION 3.20)
project(test-arch-prctl C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-arch-prctl src/main.c)
target_include_directories(test-arch-prctl PRIVATE src)
target_compile_options(test-arch-prctl PRIVATE -Wall -Wextra)
target_link_options(test-arch-prctl PRIVATE -static)

install(TARGETS test-arch-prctl RUNTIME DESTINATION usr/bin/starry-test-suit)
