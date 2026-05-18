cmake_minimum_required(VERSION 3.20)
project(bug-packet-arping C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-packet-arping src/main.c)
target_compile_options(bug-packet-arping PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-packet-arping RUNTIME DESTINATION usr/bin)
