cmake_minimum_required(VERSION 3.20)
project(test-rawmutex-handoff C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
set(THREADS_PREFER_PTHREAD_FLAG ON)

find_package(Threads REQUIRED)

add_executable(test-rawmutex-handoff src/main.c)
target_include_directories(test-rawmutex-handoff PRIVATE src)
target_compile_options(test-rawmutex-handoff PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-rawmutex-handoff PRIVATE Threads::Threads)

install(TARGETS test-rawmutex-handoff RUNTIME DESTINATION usr/bin/starry-bugfix-smp4-tests)
