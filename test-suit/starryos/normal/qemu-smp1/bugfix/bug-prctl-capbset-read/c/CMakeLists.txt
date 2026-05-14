cmake_minimum_required(VERSION 3.20)
project(bug-prctl-capbset-read C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-prctl-capbset-read src/main.c)
target_compile_options(bug-prctl-capbset-read PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-prctl-capbset-read RUNTIME DESTINATION usr/bin)
