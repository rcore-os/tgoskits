cmake_minimum_required(VERSION 3.20)

project(bug-riscv-hwprobe C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-riscv-hwprobe src/main.c)
target_compile_options(bug-riscv-hwprobe PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-riscv-hwprobe RUNTIME DESTINATION usr/bin)
