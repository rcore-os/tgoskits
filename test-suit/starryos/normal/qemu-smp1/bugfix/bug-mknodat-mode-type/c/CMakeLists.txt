cmake_minimum_required(VERSION 3.20)

project(bug-mknodat-mode-type C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-mknodat-mode-type src/main.c)
target_compile_options(bug-mknodat-mode-type PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-mknodat-mode-type RUNTIME DESTINATION usr/bin)
