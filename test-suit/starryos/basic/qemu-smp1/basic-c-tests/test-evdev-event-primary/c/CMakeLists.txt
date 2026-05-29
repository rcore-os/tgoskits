cmake_minimum_required(VERSION 3.20)
project(test-evdev-event-primary C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-evdev-event-primary src/main.c)
target_compile_options(test-evdev-event-primary PRIVATE -Wall -Wextra)

install(TARGETS test-evdev-event-primary RUNTIME DESTINATION usr/bin/starry-basic-tests)
