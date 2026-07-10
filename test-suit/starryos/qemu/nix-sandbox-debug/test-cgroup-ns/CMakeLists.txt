cmake_minimum_required(VERSION 3.20)
project(test-cgroup-ns C)

add_executable(test-cgroup-ns src/main.c)
target_link_libraries(test-cgroup-ns c)

install(TARGETS test-cgroup-ns DESTINATION /usr/bin/starry-test-suit)
