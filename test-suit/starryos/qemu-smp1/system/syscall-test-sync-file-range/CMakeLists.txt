cmake_minimum_required(VERSION 3.20)
project(test-sync-file-range C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(test-sync-file-range src/main.c)
target_include_directories(test-sync-file-range PRIVATE src)
target_compile_options(test-sync-file-range PRIVATE -Wall -Wextra -Werror)
# Build this stricter probe, but keep it out of the grouped CI pass condition
# until sync_file_range errno precedence matches Linux.
install(TARGETS test-sync-file-range RUNTIME DESTINATION usr/bin/starry-known-fail)
