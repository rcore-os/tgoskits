cmake_minimum_required(VERSION 3.20)

project(bugfix-alarm-syscall C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

add_executable(bugfix-alarm-syscall src/main.c)
target_compile_options(bugfix-alarm-syscall PRIVATE -Wall -Wextra -Werror)
target_link_libraries(bugfix-alarm-syscall PRIVATE pthread)
target_link_options(bugfix-alarm-syscall PRIVATE -static)

install(
    TARGETS bugfix-alarm-syscall
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
