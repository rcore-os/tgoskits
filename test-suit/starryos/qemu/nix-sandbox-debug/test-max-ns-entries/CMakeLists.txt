cmake_minimum_required(VERSION 3.20)
project(test-max-ns-entries C)

add_executable(test-max-ns-entries src/main.c)
target_link_libraries(test-max-ns-entries c)

install(TARGETS test-max-ns-entries DESTINATION /usr/bin/starry-test-suit)
