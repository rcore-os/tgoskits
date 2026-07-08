cmake_minimum_required(VERSION 3.20)
project(test-proc-root-cwd C)

add_executable(test-proc-root-cwd src/main.c)
target_link_libraries(test-proc-root-cwd c)

install(TARGETS test-proc-root-cwd DESTINATION /usr/bin/starry-test-suit)
