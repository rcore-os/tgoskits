cmake_minimum_required(VERSION 3.20)
project(test-proc-status-tracerpid C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-proc-status-tracerpid src/main.c)
target_compile_options(test-proc-status-tracerpid PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-proc-status-tracerpid RUNTIME DESTINATION usr/bin)
