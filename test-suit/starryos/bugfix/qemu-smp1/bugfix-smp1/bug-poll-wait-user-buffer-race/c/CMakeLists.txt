cmake_minimum_required(VERSION 3.20)
project(bug-poll-wait-user-buffer-race C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
set(THREADS_PREFER_PTHREAD_FLAG ON)

find_package(Threads REQUIRED)

add_executable(bug-poll-wait-user-buffer-race src/main.c)
target_compile_options(bug-poll-wait-user-buffer-race PRIVATE -Wall -Wextra -Werror)
target_link_libraries(bug-poll-wait-user-buffer-race PRIVATE Threads::Threads)

install(TARGETS bug-poll-wait-user-buffer-race RUNTIME DESTINATION usr/bin)
