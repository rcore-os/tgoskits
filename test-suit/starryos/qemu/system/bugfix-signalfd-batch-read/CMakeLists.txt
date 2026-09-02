cmake_minimum_required(VERSION 3.20)

project(bugfix-signalfd-batch-read C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-signalfd-batch-read src/main.c)
target_compile_options(bugfix-signalfd-batch-read PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-signalfd-batch-read RUNTIME DESTINATION usr/bin/starry-test-suit)
