cmake_minimum_required(VERSION 3.20)
project(test-tty-termios-transaction C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-tty-termios-transaction src/main.c)
target_compile_options(test-tty-termios-transaction PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-tty-termios-transaction RUNTIME DESTINATION usr/bin/starry-test-suit)
