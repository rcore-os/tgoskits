cmake_minimum_required(VERSION 3.20)
project(bugfix-clone-parent-exit-signal C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-clone-parent-exit-signal src/main.c)
target_compile_options(bugfix-clone-parent-exit-signal PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-clone-parent-exit-signal RUNTIME DESTINATION usr/bin/starry-test-suit)
