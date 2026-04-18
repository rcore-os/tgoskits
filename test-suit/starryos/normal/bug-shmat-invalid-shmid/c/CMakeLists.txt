cmake_minimum_required(VERSION 3.20)

project(bug_shmat_invalid_shmid C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug_shmat_invalid_shmid src/main.c)
target_compile_options(bug_shmat_invalid_shmid PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug_shmat_invalid_shmid RUNTIME DESTINATION usr/bin)
