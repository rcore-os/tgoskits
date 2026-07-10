cmake_minimum_required(VERSION 3.20)
project(test-ebpf-advanced C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(test-ebpf-advanced src/main.c)
target_compile_options(test-ebpf-advanced PRIVATE -Wall -Wextra -Werror)
# Build this stricter probe, but keep it out of the grouped CI pass condition
# until Starry supports BPF_OBJ_CLOSE semantics.
install(TARGETS test-ebpf-advanced RUNTIME DESTINATION usr/bin/starry-known-fail)
