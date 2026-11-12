cmake_minimum_required(VERSION 3.20)
project(bug-sched-affinity-abi C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-sched-affinity-abi src/main.c)
target_compile_options(bug-sched-affinity-abi PRIVATE -Wall -Wextra -Werror)
install(TARGETS bug-sched-affinity-abi RUNTIME DESTINATION usr/bin/starry-test-suit)
