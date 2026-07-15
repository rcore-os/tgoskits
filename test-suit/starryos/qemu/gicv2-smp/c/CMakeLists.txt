cmake_minimum_required(VERSION 3.20)
project(gicv2-smp C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(gicv2-smp src/main.c)
target_compile_options(gicv2-smp PRIVATE -Wall -Wextra -Werror)

install(TARGETS gicv2-smp RUNTIME DESTINATION usr/bin)
