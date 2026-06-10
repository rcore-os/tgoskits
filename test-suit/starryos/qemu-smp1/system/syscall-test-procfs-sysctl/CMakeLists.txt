cmake_minimum_required(VERSION 3.20)
project(test-procfs-sysctl C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-procfs-sysctl src/main.c)
target_compile_options(test-procfs-sysctl PRIVATE -Wall -Wextra)
target_link_options(test-procfs-sysctl PRIVATE -static)

install(TARGETS test-procfs-sysctl RUNTIME DESTINATION usr/bin/starry-test-suit)
