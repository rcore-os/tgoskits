cmake_minimum_required(VERSION 3.20)
project(test-ioctl C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(test-ioctl src/main.c)
target_include_directories(test-ioctl PRIVATE src)
target_compile_options(test-ioctl PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-ioctl PRIVATE)
# Build this stricter probe, but keep it out of the grouped CI pass condition
# until Starry's termios update semantics are complete.
install(TARGETS test-ioctl RUNTIME DESTINATION usr/bin/starry-known-fail)
