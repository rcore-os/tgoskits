cmake_minimum_required(VERSION 3.20)

project(sysv_msg_blocking C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(sysv_msg_blocking src/main.c)
target_compile_options(sysv_msg_blocking PRIVATE -Wall -Wextra -Werror)

install(TARGETS sysv_msg_blocking RUNTIME DESTINATION usr/bin)
