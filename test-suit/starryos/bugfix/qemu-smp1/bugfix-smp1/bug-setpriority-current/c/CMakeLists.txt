cmake_minimum_required(VERSION 3.20)

project(bug-setpriority-current C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-setpriority-current src/main.c)
target_compile_options(bug-setpriority-current PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-setpriority-current RUNTIME DESTINATION usr/bin)
