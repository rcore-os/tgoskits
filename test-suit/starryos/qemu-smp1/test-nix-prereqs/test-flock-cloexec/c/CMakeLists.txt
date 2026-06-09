cmake_minimum_required(VERSION 3.16)
project(test-flock-cloexec C)

add_executable(test-flock-cloexec src/main.c)
install(TARGETS test-flock-cloexec DESTINATION usr/bin)
