cmake_minimum_required(VERSION 3.20)

project(bugfix-proc-sysctl-writable-limits C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-proc-sysctl-writable-limits src/main.c)
target_compile_options(bugfix-proc-sysctl-writable-limits PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-proc-sysctl-writable-limits RUNTIME DESTINATION usr/bin/starry-test-suit)
