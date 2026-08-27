cmake_minimum_required(VERSION 3.20)
project(bug-chroot-parent-escape C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-chroot-parent-escape src/main.c)
target_compile_options(bug-chroot-parent-escape PRIVATE -Wall -Wextra -Werror)
install(TARGETS bug-chroot-parent-escape RUNTIME DESTINATION usr/bin/starry-test-suit)
