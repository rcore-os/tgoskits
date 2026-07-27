cmake_minimum_required(VERSION 3.20)
project(test-mq-signal-handoff C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-mq-signal-handoff src/main.c)
target_include_directories(test-mq-signal-handoff PRIVATE src)
target_compile_options(test-mq-signal-handoff PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-mq-signal-handoff PRIVATE pthread)
target_link_options(test-mq-signal-handoff PRIVATE -static)

install(TARGETS test-mq-signal-handoff RUNTIME DESTINATION usr/bin/starry-test-suit)
