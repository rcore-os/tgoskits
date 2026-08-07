cmake_minimum_required(VERSION 3.20)
project(bugfix-mount-setattr C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-mount-setattr src/main.c)
target_compile_options(bugfix-mount-setattr PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-mount-setattr RUNTIME DESTINATION usr/bin/starry-test-suit)
