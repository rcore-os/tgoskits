cmake_minimum_required(VERSION 3.20)
project(bugfix-oom-score-adj C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-oom-score-adj src/main.c)
target_compile_options(bugfix-oom-score-adj PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-oom-score-adj RUNTIME DESTINATION usr/bin/starry-test-suit)
