cmake_minimum_required(VERSION 3.20)

project(bugfix-proc-pid-exe-readlink C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-proc-pid-exe-readlink src/main.c)
target_compile_options(bugfix-proc-pid-exe-readlink PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-proc-pid-exe-readlink RUNTIME DESTINATION usr/bin/starry-test-suit)
