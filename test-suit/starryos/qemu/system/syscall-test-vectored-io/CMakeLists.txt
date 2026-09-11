cmake_minimum_required(VERSION 3.20)

project(test-vectored-io C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-vectored-io src/main.c)
target_compile_options(test-vectored-io PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-vectored-io RUNTIME DESTINATION usr/bin/starry-test-suit)

find_package(Threads REQUIRED)
add_executable(test-pipe-writev src/pipe_writev.c)
target_compile_options(test-pipe-writev PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-pipe-writev PRIVATE Threads::Threads)
install(TARGETS test-pipe-writev RUNTIME DESTINATION usr/bin/starry-test-suit)
