cmake_minimum_required(VERSION 3.20)
project(bug-proc-net-arp C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-proc-net-arp src/main.c)
target_compile_options(bug-proc-net-arp PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-proc-net-arp RUNTIME DESTINATION usr/bin)
