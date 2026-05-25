cmake_minimum_required(VERSION 3.20)
project(bug-unshare-uts C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-unshare-uts src/main.c)
target_compile_options(bug-unshare-uts PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-unshare-uts RUNTIME DESTINATION usr/bin)
