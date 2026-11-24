cmake_minimum_required(VERSION 3.20)
project(test-proc-link-access C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

find_package(Threads REQUIRED)
add_executable(test-proc-link-access src/main.c)
target_compile_options(test-proc-link-access PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-proc-link-access PRIVATE Threads::Threads)
install(TARGETS test-proc-link-access RUNTIME DESTINATION usr/bin/starry-test-suit)
