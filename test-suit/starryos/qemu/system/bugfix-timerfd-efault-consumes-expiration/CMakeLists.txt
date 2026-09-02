cmake_minimum_required(VERSION 3.20)

project(bugfix-timerfd-efault-consumes-expiration C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bugfix-timerfd-efault-consumes-expiration src/main.c)
target_compile_options(
    bugfix-timerfd-efault-consumes-expiration
    PRIVATE -Wall -Wextra -Werror
)

install(
    TARGETS bugfix-timerfd-efault-consumes-expiration
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
