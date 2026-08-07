cmake_minimum_required(VERSION 3.20)
project(bugfix-fstatvfs-directory C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-fstatvfs-directory src/main.c)
target_compile_options(bugfix-fstatvfs-directory PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-fstatvfs-directory RUNTIME DESTINATION usr/bin/starry-test-suit)
