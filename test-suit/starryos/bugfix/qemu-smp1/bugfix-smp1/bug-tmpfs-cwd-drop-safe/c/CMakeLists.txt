cmake_minimum_required(VERSION 3.20)
project(bug-tmpfs-cwd-drop-safe C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-tmpfs-cwd-drop-safe src/main.c)
target_compile_options(bug-tmpfs-cwd-drop-safe PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-tmpfs-cwd-drop-safe RUNTIME DESTINATION usr/bin)
