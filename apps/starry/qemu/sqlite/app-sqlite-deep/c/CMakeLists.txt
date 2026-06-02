cmake_minimum_required(VERSION 3.20)
project(app-sqlite-deep C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

set(CMAKE_FIND_LIBRARY_SUFFIXES .a)

find_package(SQLite3 REQUIRED)

add_executable(app-sqlite-deep src/main.c)
target_compile_options(app-sqlite-deep PRIVATE -Wall -Wextra -Werror)
target_link_libraries(app-sqlite-deep SQLite::SQLite3)
target_link_options(app-sqlite-deep PRIVATE -static)

install(TARGETS app-sqlite-deep RUNTIME DESTINATION usr/bin)
