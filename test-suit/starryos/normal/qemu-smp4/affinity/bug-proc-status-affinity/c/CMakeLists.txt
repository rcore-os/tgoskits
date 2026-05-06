cmake_minimum_required(VERSION 3.20)

project(test_proc_status_affinity C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test_proc_status_affinity src/main.c)
target_compile_options(test_proc_status_affinity PRIVATE -Wall -Wextra -Werror)

install(TARGETS test_proc_status_affinity RUNTIME DESTINATION usr/bin)
