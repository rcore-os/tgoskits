cmake_minimum_required(VERSION 3.20)
project(mm-transition-safety C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(mm-transition-safety src/main.c)
target_compile_options(mm-transition-safety PRIVATE -Wall -Wextra -Werror)
target_link_libraries(mm-transition-safety PRIVATE pthread)

install(TARGETS mm-transition-safety RUNTIME DESTINATION usr/bin/starry-test-suit)
