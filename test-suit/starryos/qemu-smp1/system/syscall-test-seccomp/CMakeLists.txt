cmake_minimum_required(VERSION 3.20)
project(test-seccomp C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-seccomp src/main.c)
target_compile_options(test-seccomp PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-seccomp PRIVATE pthread)

install(TARGETS test-seccomp RUNTIME DESTINATION usr/bin/starry-test-suit)
