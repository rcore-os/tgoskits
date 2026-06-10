cmake_minimum_required(VERSION 3.20)

project(bug-af-inet6-v4mapped C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-af-inet6-v4mapped src/main.c)
target_compile_options(bug-af-inet6-v4mapped PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-af-inet6-v4mapped RUNTIME DESTINATION usr/bin/starry-test-suit)
