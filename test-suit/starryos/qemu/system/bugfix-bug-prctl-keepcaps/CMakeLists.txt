cmake_minimum_required(VERSION 3.20)
project(bug-prctl-keepcaps C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-prctl-keepcaps src/main.c)
target_compile_options(bug-prctl-keepcaps PRIVATE -Wall -Wextra -Werror -pthread)
target_link_options(bug-prctl-keepcaps PRIVATE -pthread)

install(TARGETS bug-prctl-keepcaps RUNTIME DESTINATION usr/bin/starry-test-suit)
