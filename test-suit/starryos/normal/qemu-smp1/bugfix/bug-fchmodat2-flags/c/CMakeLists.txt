cmake_minimum_required(VERSION 3.20)

project(bug-fchmodat2-flags C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-fchmodat2-flags src/main.c)
target_compile_options(bug-fchmodat2-flags PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-fchmodat2-flags RUNTIME DESTINATION usr/bin)
