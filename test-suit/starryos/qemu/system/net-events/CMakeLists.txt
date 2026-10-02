cmake_minimum_required(VERSION 3.20)
project(net-events C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(net-events src/main.c)
target_compile_options(net-events PRIVATE -Wall -Wextra -Werror)
install(TARGETS net-events RUNTIME DESTINATION usr/bin/starry-test-suit)
