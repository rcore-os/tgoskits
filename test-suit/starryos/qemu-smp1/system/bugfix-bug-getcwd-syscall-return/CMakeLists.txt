cmake_minimum_required(VERSION 3.20)
project(bug-getcwd-syscall-return C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-getcwd-syscall-return src/main.c)
target_compile_options(bug-getcwd-syscall-return PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-getcwd-syscall-return RUNTIME DESTINATION usr/bin/starry-test-suit)
