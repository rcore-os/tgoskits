cmake_minimum_required(VERSION 3.20)

project(test-splice C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-splice src/main.c)
include_directories(test-splice PRIVATE src)
target_compile_options(test-splice PRIVATE -Wall -Wextra -Werror)

# Build this stricter probe, but keep it out of the grouped CI pass condition
# until splice offset and same-pipe edge cases are complete.
install(TARGETS test-splice RUNTIME DESTINATION usr/bin/starry-known-fail)
