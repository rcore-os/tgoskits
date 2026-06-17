cmake_minimum_required(VERSION 3.20)
project(test-msgsnd C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-msgsnd src/main.c)
target_include_directories(test-msgsnd PRIVATE src)
target_compile_options(test-msgsnd PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-msgsnd RUNTIME DESTINATION usr/bin/starry-test-suit)
