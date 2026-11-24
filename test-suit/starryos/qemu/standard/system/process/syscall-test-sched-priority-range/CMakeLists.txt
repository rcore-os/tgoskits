cmake_minimum_required(VERSION 3.20)
project(syscall-test-sched-priority-range C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(syscall-test-sched-priority-range src/main.c)
target_compile_options(syscall-test-sched-priority-range PRIVATE -Wall -Wextra -Werror)

install(TARGETS syscall-test-sched-priority-range RUNTIME DESTINATION usr/bin/starry-test-suit)
