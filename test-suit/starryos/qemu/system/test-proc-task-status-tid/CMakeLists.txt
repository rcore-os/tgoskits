cmake_minimum_required(VERSION 3.16)
project(test-proc-task-status-tid C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

starry_arch_filtered_executable(test-proc-task-status-tid "riscv64" "test-proc-task-status-tid skipped on unsupported architecture" src/main.c)
target_compile_options(test-proc-task-status-tid PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-proc-task-status-tid PRIVATE pthread)
install(TARGETS test-proc-task-status-tid RUNTIME DESTINATION usr/bin/starry-test-suit)
