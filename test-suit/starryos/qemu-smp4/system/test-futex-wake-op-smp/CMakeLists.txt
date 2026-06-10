cmake_minimum_required(VERSION 3.20)
project(test-futex-wake-op-smp C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-futex-wake-op-smp src/main.c)
target_compile_options(test-futex-wake-op-smp PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-futex-wake-op-smp PRIVATE pthread)

install(TARGETS test-futex-wake-op-smp RUNTIME DESTINATION usr/bin/starry-test-suit)
