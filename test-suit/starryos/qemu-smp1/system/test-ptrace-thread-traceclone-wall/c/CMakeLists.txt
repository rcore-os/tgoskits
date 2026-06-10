cmake_minimum_required(VERSION 3.16)
project(test-ptrace-thread-traceclone-wall C)
include(${CMAKE_CURRENT_LIST_DIR}/../../common/starry_arch_filter.cmake)

starry_arch_filtered_executable(test-ptrace-thread-traceclone-wall "riscv64" "test-ptrace-thread-traceclone-wall skipped on unsupported architecture" src/main.c)
target_compile_options(test-ptrace-thread-traceclone-wall PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-ptrace-thread-traceclone-wall PRIVATE pthread)
install(TARGETS test-ptrace-thread-traceclone-wall RUNTIME DESTINATION usr/bin/starry-test-suit)
