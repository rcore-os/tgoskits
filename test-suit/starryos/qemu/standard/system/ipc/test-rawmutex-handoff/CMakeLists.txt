cmake_minimum_required(VERSION 3.20)
project(test-rawmutex-handoff C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-rawmutex-handoff src/main.c)
target_include_directories(test-rawmutex-handoff PRIVATE src)
target_compile_options(test-rawmutex-handoff PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-rawmutex-handoff PRIVATE pthread)

install(TARGETS test-rawmutex-handoff RUNTIME DESTINATION usr/bin/starry-test-suit)
