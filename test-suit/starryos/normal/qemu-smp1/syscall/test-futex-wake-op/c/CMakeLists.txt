cmake_minimum_required(VERSION 3.20)
project(test-futex-wake-op C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-futex-wake-op src/main.c)
target_include_directories(
    test-futex-wake-op
    PRIVATE
    src
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common
)
target_compile_options(test-futex-wake-op PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-futex-wake-op PRIVATE pthread)

install(TARGETS test-futex-wake-op RUNTIME DESTINATION usr/bin/starry-test-suit)
