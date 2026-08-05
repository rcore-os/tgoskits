cmake_minimum_required(VERSION 3.20)
project(test-cgroup-ns C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-cgroup-ns src/main.c)
target_compile_options(test-cgroup-ns PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-cgroup-ns RUNTIME DESTINATION usr/bin/starry-test-suit)
