cmake_minimum_required(VERSION 3.20)
project(bugfix-devpts-new-mount-api C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-devpts-new-mount-api src/main.c)
target_compile_options(bugfix-devpts-new-mount-api PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-devpts-new-mount-api RUNTIME DESTINATION usr/bin/starry-test-suit)
