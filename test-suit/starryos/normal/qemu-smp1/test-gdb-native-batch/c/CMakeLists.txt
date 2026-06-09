cmake_minimum_required(VERSION 3.20)
project(test-gdb-native-batch C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# Dynamically-linked inferior: the program debugged through ld-musl startup.
add_executable(test-gdb-native-batch-target src/main.c)
target_compile_options(test-gdb-native-batch-target PRIVATE -Wall -Wextra -Werror -O0 -g)

# Raw-ptrace tracer: drives ptrace(2) directly, no dependency on real gdb.
add_executable(test-gdb-native-batch-tracer src/tracer.c)
target_compile_options(test-gdb-native-batch-tracer PRIVATE -Wall -Wextra -Werror -O0 -g)

install(TARGETS test-gdb-native-batch-target test-gdb-native-batch-tracer
        RUNTIME DESTINATION usr/bin)
