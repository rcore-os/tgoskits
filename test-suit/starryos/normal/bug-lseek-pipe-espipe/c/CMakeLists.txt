cmake_minimum_required(VERSION 3.20)

project(bug_lseek_pipe_espipe C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug_lseek_pipe_espipe src/main.c)
target_compile_options(bug_lseek_pipe_espipe PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug_lseek_pipe_espipe RUNTIME DESTINATION usr/bin)
