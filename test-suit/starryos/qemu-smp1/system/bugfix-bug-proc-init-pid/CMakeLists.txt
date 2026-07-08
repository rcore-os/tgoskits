cmake_minimum_required(VERSION 3.20)

project(bug-proc-init-pid C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-proc-init-pid src/main.c)
target_compile_options(bug-proc-init-pid PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-proc-init-pid RUNTIME DESTINATION usr/bin/starry-test-suit)
