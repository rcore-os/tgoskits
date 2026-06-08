cmake_minimum_required(VERSION 3.16)
project(test-proc-task-status-tid C)

add_executable(test-proc-task-status-tid src/main.c)
target_compile_options(test-proc-task-status-tid PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-proc-task-status-tid PRIVATE pthread)
install(TARGETS test-proc-task-status-tid RUNTIME DESTINATION usr/bin)
