cmake_minimum_required(VERSION 3.20)
project(bug-proc-stat-monotonic C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-proc-stat-monotonic src/main.c)
target_compile_options(bug-proc-stat-monotonic PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-proc-stat-monotonic RUNTIME DESTINATION usr/bin)
