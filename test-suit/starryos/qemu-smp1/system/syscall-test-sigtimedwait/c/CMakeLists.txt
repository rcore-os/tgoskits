cmake_minimum_required(VERSION 3.20)
project(test-sigtimedwait C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-sigtimedwait src/main.c)
target_compile_options(test-sigtimedwait PRIVATE -Wall -Wextra -Werror)

# Build this stricter probe, but keep it out of the grouped CI pass condition
# until queued signal delivery preserves siginfo.
install(TARGETS test-sigtimedwait RUNTIME DESTINATION usr/bin/starry-known-fail)
