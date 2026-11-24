cmake_minimum_required(VERSION 3.20)

project(bugfix-tcp-congestion-sockopt C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-tcp-congestion-sockopt src/main.c)
target_compile_options(bugfix-tcp-congestion-sockopt PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-tcp-congestion-sockopt RUNTIME DESTINATION usr/bin/starry-test-suit)
