cmake_minimum_required(VERSION 3.20)
project(test-pgkill-session C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-pgkill-session src/main.c)
target_include_directories(test-pgkill-session PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../../common)
target_compile_options(test-pgkill-session PRIVATE -Wall -Wextra -Werror)
target_link_options(test-pgkill-session PRIVATE -static)

install(TARGETS test-pgkill-session RUNTIME DESTINATION usr/bin/starry-test-suit)
