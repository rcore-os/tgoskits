cmake_minimum_required(VERSION 3.20)
project(bug-prctl-set-vma-anon-name C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-prctl-set-vma-anon-name src/main.c)
target_compile_options(bug-prctl-set-vma-anon-name PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-prctl-set-vma-anon-name RUNTIME DESTINATION usr/bin)
