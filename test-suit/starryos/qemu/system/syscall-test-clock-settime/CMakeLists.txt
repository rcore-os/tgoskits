cmake_minimum_required(VERSION 3.20)

project(syscall-test-clock-settime C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(syscall-test-clock-settime src/main.c)
target_compile_options(syscall-test-clock-settime PRIVATE -Wall -Wextra -Werror)
target_link_options(syscall-test-clock-settime PRIVATE -static)

install(
    TARGETS syscall-test-clock-settime
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
