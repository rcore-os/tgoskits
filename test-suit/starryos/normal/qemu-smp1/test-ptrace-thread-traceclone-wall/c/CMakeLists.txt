cmake_minimum_required(VERSION 3.16)
project(test-ptrace-thread-traceclone-wall C)

add_executable(test-ptrace-thread-traceclone-wall src/main.c)
target_compile_options(test-ptrace-thread-traceclone-wall PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-ptrace-thread-traceclone-wall PRIVATE pthread)
install(TARGETS test-ptrace-thread-traceclone-wall RUNTIME DESTINATION usr/bin)
