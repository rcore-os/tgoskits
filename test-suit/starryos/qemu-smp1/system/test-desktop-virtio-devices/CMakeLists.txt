cmake_minimum_required(VERSION 3.20)
project(test-desktop-virtio-devices C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-desktop-virtio-devices src/main.c)
target_include_directories(test-desktop-virtio-devices PRIVATE src ../common)
target_compile_options(test-desktop-virtio-devices PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-desktop-virtio-devices RUNTIME DESTINATION usr/bin/starry-test-suit)
