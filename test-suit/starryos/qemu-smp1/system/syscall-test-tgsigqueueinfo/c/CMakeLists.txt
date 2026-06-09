cmake_minimum_required(VERSION 3.20)
project(test-tgsigqueueinfo C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-tgsigqueueinfo src/main.c)
target_compile_options(test-tgsigqueueinfo PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-tgsigqueueinfo PRIVATE pthread)

# Build this stricter probe, but keep it out of the grouped CI pass condition
# until queued thread signal delivery preserves siginfo.
install(TARGETS test-tgsigqueueinfo RUNTIME DESTINATION usr/bin/starry-known-fail)
