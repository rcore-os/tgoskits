cmake_minimum_required(VERSION 3.20)

project(test-preadv-pwritev2 C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-preadv-pwritev2 src/main.c)
target_compile_options(test-preadv-pwritev2 PRIVATE -Wall -Wextra -Werror)

# Build this stricter probe, but keep it out of the grouped CI pass condition
# until O_APPEND pwritev/pwritev2 semantics are complete.
install(TARGETS test-preadv-pwritev2 RUNTIME DESTINATION usr/bin/starry-known-fail)
