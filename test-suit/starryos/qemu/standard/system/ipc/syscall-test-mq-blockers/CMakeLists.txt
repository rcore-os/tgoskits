cmake_minimum_required(VERSION 3.20)
project(test-mq-blockers C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-mq-blockers src/main.c)
target_include_directories(test-mq-blockers PRIVATE src)
target_compile_options(test-mq-blockers PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-mq-blockers PRIVATE pthread)
target_link_options(test-mq-blockers PRIVATE -static)

install(TARGETS test-mq-blockers RUNTIME DESTINATION usr/bin/starry-test-suit)
