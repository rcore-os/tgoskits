cmake_minimum_required(VERSION 3.20)
project(timer-preemption-x86 C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(timer-preemption-x86 src/main.c)
target_compile_options(timer-preemption-x86 PRIVATE -Wall -Wextra -Werror)

install(TARGETS timer-preemption-x86 RUNTIME DESTINATION usr/bin)
