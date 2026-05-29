cmake_minimum_required(VERSION 3.20)
project(test-fault-thread-routing C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
set(THREADS_PREFER_PTHREAD_FLAG ON)

find_package(Threads REQUIRED)

add_executable(test-fault-thread-routing src/main.c)
target_include_directories(test-fault-thread-routing PRIVATE src)
target_compile_options(test-fault-thread-routing PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-fault-thread-routing PRIVATE Threads::Threads)

install(TARGETS test-fault-thread-routing RUNTIME DESTINATION usr/bin/starry-test-suit)
