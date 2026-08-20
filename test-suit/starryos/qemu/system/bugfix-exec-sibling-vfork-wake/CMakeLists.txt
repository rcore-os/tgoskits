cmake_minimum_required(VERSION 3.20)
project(bugfix-exec-sibling-vfork-wake C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bugfix-exec-sibling-vfork-wake src/main.c)
target_compile_options(
    bugfix-exec-sibling-vfork-wake
    PRIVATE -Wall -Wextra -Werror -pthread
)
target_link_options(bugfix-exec-sibling-vfork-wake PRIVATE -pthread)
install(
    TARGETS bugfix-exec-sibling-vfork-wake
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
