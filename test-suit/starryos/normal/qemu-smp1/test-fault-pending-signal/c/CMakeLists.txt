cmake_minimum_required(VERSION 3.20)
project(test-fault-pending-signal C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-fault-pending-signal src/main.c)
target_include_directories(test-fault-pending-signal PRIVATE src)
target_compile_options(test-fault-pending-signal PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-fault-pending-signal PRIVATE pthread)

install(TARGETS test-fault-pending-signal RUNTIME DESTINATION usr/bin)
