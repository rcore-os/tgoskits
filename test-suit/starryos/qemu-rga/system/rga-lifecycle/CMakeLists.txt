cmake_minimum_required(VERSION 3.20)
project(rga-lifecycle C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(rga-lifecycle src/main.c)
target_compile_options(rga-lifecycle PRIVATE -Wall -Wextra -Werror)
target_link_libraries(rga-lifecycle PRIVATE pthread)

install(TARGETS rga-lifecycle RUNTIME DESTINATION usr/bin/starry-test-suit)
