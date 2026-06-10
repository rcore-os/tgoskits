cmake_minimum_required(VERSION 3.20)
project(test-gdb-native-batch C)
include(${CMAKE_CURRENT_LIST_DIR}/../../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# Dynamically-linked inferior: the program debugged through ld-musl startup.
# Goes to usr/bin so the tracer can execve it; NOT in starry-test-suit to
# avoid being run standalone.
starry_arch_filtered_executable(test-gdb-native-batch-target "x86_64" "test-gdb-native-batch-target skipped on unsupported architecture" src/main.c)
target_compile_options(test-gdb-native-batch-target PRIVATE -Wall -Wextra -Werror -O0 -g)
install(TARGETS test-gdb-native-batch-target RUNTIME DESTINATION usr/bin)

# Raw-ptrace tracer: driven by the grouped system test runner.
starry_arch_filtered_executable(test-gdb-native-batch-tracer "x86_64" "test-gdb-native-batch skipped on unsupported architecture" src/tracer.c)
target_compile_options(test-gdb-native-batch-tracer PRIVATE -Wall -Wextra -Werror -O0 -g)
install(TARGETS test-gdb-native-batch-tracer RUNTIME DESTINATION usr/bin/starry-test-suit)
