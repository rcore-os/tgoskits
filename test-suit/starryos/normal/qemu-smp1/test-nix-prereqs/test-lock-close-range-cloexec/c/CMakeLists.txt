cmake_minimum_required(VERSION 3.16)
project(test-lock-close-range-cloexec C)

add_executable(test-lock-close-range-cloexec src/main.c)
install(TARGETS test-lock-close-range-cloexec DESTINATION usr/bin)
