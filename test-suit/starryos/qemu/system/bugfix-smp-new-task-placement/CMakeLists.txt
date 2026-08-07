cmake_minimum_required(VERSION 3.20)
project(bugfix-smp-new-task-placement C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-smp-new-task-placement src/main.c)
target_compile_options(bugfix-smp-new-task-placement PRIVATE -Wall -Wextra -Werror)

install(
    TARGETS bugfix-smp-new-task-placement
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
