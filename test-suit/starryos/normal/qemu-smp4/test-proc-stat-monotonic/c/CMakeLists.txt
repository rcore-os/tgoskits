cmake_minimum_required(VERSION 3.20)
project(test-proc-stat-monotonic C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-proc-stat-monotonic src/main.c)
target_compile_options(test-proc-stat-monotonic PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-proc-stat-monotonic RUNTIME DESTINATION usr/bin)
