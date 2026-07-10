cmake_minimum_required(VERSION 3.20)
project(test-proc-environ C)

add_executable(test-proc-environ src/main.c)
target_link_libraries(test-proc-environ c)

install(TARGETS test-proc-environ DESTINATION /usr/bin/starry-test-suit)
