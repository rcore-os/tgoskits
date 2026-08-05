cmake_minimum_required(VERSION 3.20)
project(test-ptrace-seize-syscall-stop C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(
    test-ptrace-seize-syscall-stop
    "x86_64"
    "test-ptrace-seize-syscall-stop skipped on unsupported architecture"
    src/main.c
)
target_compile_options(test-ptrace-seize-syscall-stop PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-ptrace-seize-syscall-stop PRIVATE pthread)
install(TARGETS test-ptrace-seize-syscall-stop RUNTIME DESTINATION usr/bin/starry-test-suit)
