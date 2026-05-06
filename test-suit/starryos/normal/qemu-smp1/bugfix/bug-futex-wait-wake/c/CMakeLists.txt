cmake_minimum_required(VERSION 3.20)
project(bug-futex-wait-wake C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
set(THREADS_PREFER_PTHREAD_FLAG ON)

find_package(Threads REQUIRED)

add_executable(bug-futex-wait-wake src/main.c)
target_compile_options(bug-futex-wait-wake PRIVATE -Wall -Wextra -Werror)
target_link_libraries(bug-futex-wait-wake PRIVATE Threads::Threads)

install(TARGETS bug-futex-wait-wake RUNTIME DESTINATION usr/bin)
