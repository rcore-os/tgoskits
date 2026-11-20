cmake_minimum_required(VERSION 3.20)
project(starry-tty-inherited-baudrate C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(tty-inherited-baudrate src/main.c)
target_compile_options(tty-inherited-baudrate PRIVATE -Wall -Wextra -Werror)
install(TARGETS tty-inherited-baudrate RUNTIME DESTINATION usr/bin)
