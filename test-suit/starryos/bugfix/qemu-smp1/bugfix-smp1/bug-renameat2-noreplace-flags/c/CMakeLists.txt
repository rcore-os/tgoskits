cmake_minimum_required(VERSION 3.20)
project(bug-renameat2-noreplace-flags C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-renameat2-noreplace-flags src/main.c)
target_compile_options(bug-renameat2-noreplace-flags PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-renameat2-noreplace-flags RUNTIME DESTINATION usr/bin)
