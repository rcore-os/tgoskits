cmake_minimum_required(VERSION 3.20)
project(test-smp-thread-distribution C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-smp-thread-distribution src/main.c)
target_include_directories(test-smp-thread-distribution PRIVATE src)
target_compile_options(test-smp-thread-distribution PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-smp-thread-distribution PRIVATE pthread)

install(TARGETS test-smp-thread-distribution RUNTIME DESTINATION usr/bin/starry-test-suit)
